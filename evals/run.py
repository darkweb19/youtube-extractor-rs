#!/usr/bin/env python3
"""Run deterministic project scenarios against the Rust implementation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def load_json(path):
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def result_statuses(rows):
    if not isinstance(rows, list) or not rows:
        raise ValueError("results must be a nonempty array of case rows")
    statuses = {}
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError("each result must be a case object")
        case_id = row.get("id")
        status = row.get("status")
        if not isinstance(case_id, str) or not case_id or case_id in statuses:
            raise ValueError("result IDs must be unique nonempty strings")
        if status not in ("passed", "failed"):
            raise ValueError("result status must be passed or failed")
        statuses[case_id] = status
    return statuses


def compare(current, baseline):
    if not isinstance(baseline, dict):
        raise ValueError("baseline must be a report object")
    if baseline.get("schema_version") != 1 or baseline.get("completed") is not True:
        raise ValueError("baseline must be a completed report with schema_version 1")
    if current["suite_digest"] != baseline.get("suite_digest"):
        raise ValueError("baseline uses a different scenario corpus")
    old = result_statuses(baseline.get("results"))
    new = result_statuses(current["results"])
    if old.keys() != new.keys():
        raise ValueError("baseline must contain exactly the selected case IDs")
    return [case for case in new if old[case] == "passed" and new[case] != "passed"]


def select_cases(cases, selected):
    ids = [case["id"] for case in cases]
    if not ids or any(not value for value in ids) or len(ids) != len(set(ids)):
        raise ValueError("scenario IDs must be unique and nonempty")
    unknown = set(selected) - set(ids)
    if unknown:
        raise ValueError("unknown case IDs: " + ", ".join(sorted(unknown)))
    return [case for case in cases if not selected or case["id"] in selected]


def command(args, log, env=None):
    result = subprocess.run(args, cwd=ROOT, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            timeout=600, check=False)
    log.append("$ " + " ".join(args) + "\n" + result.stdout)
    return result


def run_core(cases, log):
    if not cases:
        return []
    with tempfile.TemporaryDirectory(prefix="extractor-evals-") as temp:
        report = Path(temp) / "core.json"
        env = os.environ.copy()
        env["EXTRACTOR_EVAL_CASES"] = json.dumps([case["id"] for case in cases])
        env["EXTRACTOR_EVAL_REPORT"] = str(report)
        result = command(["cargo", "test", "--locked", "--test", "evals", "--",
                          "--exact", "evaluate_scenarios"], log, env)
        if not report.is_file():
            raise ValueError("Rust evaluator did not produce a report; inspect cargo.log")
        rows = load_json(report)
        statuses = result_statuses(rows)
        if statuses.keys() != {case["id"] for case in cases}:
            raise ValueError("Rust evaluator returned an incomplete or duplicate case set")
        if result.returncode and all(status == "passed" for status in statuses.values()):
            raise ValueError("Cargo failed despite passing cases; inspect cargo.log")
        return rows


def run_remote(cases, log):
    if not cases:
        return []
    listed = command(["cargo", "test", "--locked", "--lib", "remote::tests::worker_tests::eval_",
                      "--", "--list"], log)
    if listed.returncode:
        raise ValueError("could not enumerate remote evals; inspect cargo.log")
    available = {line.removesuffix(": test") for line in listed.stdout.splitlines()
                 if line.endswith(": test")}
    rows = []
    for case in cases:
        if case["test"] not in available:
            raise ValueError("remote eval test is missing: " + case["test"])
        result = command(["cargo", "test", "--locked", "--lib", case["test"], "--", "--exact"], log)
        passed = result.returncode == 0 and "1 passed; 0 failed" in result.stdout
        rows.append({"id": case["id"], "source": case["source"],
                     "status": "passed" if passed else "failed",
                     "detail": "scenario checks passed" if passed else "remote scenario failed; inspect cargo.log"})
    return rows


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="report directory (use work/evals)")
    parser.add_argument("--suite", choices=["all", "core", "remote"], default="all")
    parser.add_argument("--case", action="append", default=[], help="exact case ID; repeat to select several")
    parser.add_argument("--baseline", type=Path, help="previous results.json with identical cases")
    args = parser.parse_args(argv)
    log = []
    try:
        core_path = ROOT / "evals/cases.json"
        remote_path = ROOT / "evals/remote-cases.json"
        core = load_json(core_path)
        remote = load_json(remote_path)
        corpus = core + remote
        select_cases(corpus, [])
        available = (core if args.suite == "core" else remote if args.suite == "remote" else corpus)
        selected = select_cases(available, args.case)
        digest = hashlib.sha256(core_path.read_bytes() + b"\0" + remote_path.read_bytes()).hexdigest()
        baseline = load_json(args.baseline) if args.baseline else None
        report = {"schema_version": 1, "completed": True, "suite_digest": digest,
                  "results": [{"id": case["id"], "status": "passed"} for case in selected]}
        if args.baseline is not None:
            compare(report, baseline)
        args.out.mkdir(parents=True, exist_ok=True)
        # Each run must create fresh evidence, even when reusing an output directory.
        for name in ["results.json", "summary.md", "cargo.log", "error.txt"]:
            (args.out / name).unlink(missing_ok=True)
        selected_ids = {case["id"] for case in selected}
        rows = run_core([case for case in core if case["id"] in selected_ids], log)
        rows += run_remote([case for case in remote if case["id"] in selected_ids], log)
        if not rows:
            raise ValueError("no scenarios ran")
        report["results"] = rows
        report["passed"] = sum(row["status"] == "passed" for row in rows)
        report["failed"] = len(rows) - report["passed"]
        report["regressions"] = compare(report, baseline) if args.baseline is not None else []
        (args.out / "results.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        lines = ["# Deterministic evaluation", "", f"{report['passed']}/{len(rows)} cases passed.", "",
                 "| Case | Result |", "| --- | --- |"]
        lines.extend(f"| {row['id']} | {row['status']} |" for row in rows)
        if args.baseline is not None:
            lines += ["", f"Regressions against baseline: {len(report['regressions'])}."]
        (args.out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
        print(f"{report['passed']}/{len(rows)} cases passed; reports: {args.out}")
        return 1 if report["failed"] or report["regressions"] else 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        args.out.mkdir(parents=True, exist_ok=True)
        (args.out / "error.txt").write_text(str(error) + "\n", encoding="utf-8")
        (args.out / "results.json").write_text(json.dumps({"schema_version": 1, "completed": False,
                                                          "error": str(error)}, indent=2) + "\n", encoding="utf-8")
        (args.out / "summary.md").write_text("# Evaluation incomplete\n\nSee error.txt and cargo.log.\n", encoding="utf-8")
        print("Evaluation could not complete: " + str(error), file=sys.stderr)
        return 2
    finally:
        if args.out.is_dir():
            (args.out / "cargo.log").write_text("\n".join(log), encoding="utf-8")


if __name__ == "__main__":
    sys.exit(main())
