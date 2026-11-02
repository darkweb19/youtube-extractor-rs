"""Grader/report guardrails; never starts Cargo or an external service."""
import unittest
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
from unittest.mock import patch
from run import compare, select_cases, run_core, run_remote, main, result_statuses


class RunnerTests(unittest.TestCase):
    def report(self, rows, digest="same"):
        return {"schema_version": 1, "completed": True, "suite_digest": digest,
                "results": [{"id": name, "status": status} for name, status in rows]}

    def test_select_requires_real_unique_nonempty_cases(self):
        cases = [{"id": "control"}, {"id": "failure"}]
        self.assertEqual(select_cases(cases, ["failure"]), cases[1:])
        for corpus, selected in [(cases, ["typo"]), ([], []), (cases + cases, [])]:
            with self.assertRaises(ValueError):
                select_cases(corpus, selected)

    def test_baseline_comparison_records_regressions_and_improvements(self):
        old = self.report([("regression", "passed"), ("improvement", "failed")])
        new = self.report([("regression", "failed"), ("improvement", "passed")])
        self.assertEqual(compare(new, old), ["regression"])

    def test_baselines_cannot_hide_changed_or_missing_cases(self):
        current = self.report([("a", "passed")])
        for old in [self.report([("b", "passed")]), self.report([("a", "passed")], "changed"),
                    self.report([("a", "passed"), ("a", "failed")]), {"schema_version": 0},
                    [], None, 42, "not a report"]:
            with self.assertRaises(ValueError):
                compare(current, old)

    @patch("run.command")
    def test_missing_remote_test_cannot_pass_as_zero_tests(self, command):
        command.return_value = SimpleNamespace(returncode=0, stdout="0 tests, 0 benchmarks\n")
        with self.assertRaisesRegex(ValueError, "test is missing"):
            run_remote([{"id": "case", "test": "remote::missing"}], [])

    @patch("run.command")
    def test_cargo_success_without_core_report_is_not_a_pass(self, command):
        command.return_value = SimpleNamespace(returncode=0, stdout="0 tests\n")
        with self.assertRaisesRegex(ValueError, "did not produce a report"):
            run_core([{"id": "case"}], [])

    @patch("run.command")
    def test_exact_remote_test_must_actually_run(self, command):
        command.side_effect = [SimpleNamespace(returncode=0, stdout="remote::case: test\n"),
                               SimpleNamespace(returncode=0, stdout="0 tests\n")]
        rows = run_remote([{"id": "case", "test": "remote::case", "source": "fixture"}], [])
        self.assertEqual(rows[0]["status"], "failed")

    def fixture_root(self, temp):
        root = Path(temp)
        (root / "evals").mkdir()
        (root / "evals/cases.json").write_text(json.dumps([{"id": "control", "source": "synthetic contract"}]))
        (root / "evals/remote-cases.json").write_text("[]")
        return root

    def test_main_writes_case_failure_and_returns_one(self):
        with tempfile.TemporaryDirectory() as temp:
            root = self.fixture_root(temp)
            rows = [{"id": "control", "source": "synthetic contract", "status": "failed", "detail": "expected rejection"}]
            with patch("run.ROOT", root), patch("run.run_core", return_value=rows), patch("run.run_remote", return_value=[]):
                self.assertEqual(main(["--out", str(root / "out"), "--suite", "core"]), 1)
            report = json.loads((root / "out/results.json").read_text())
            self.assertTrue(report["completed"])
            self.assertEqual(report["failed"], 1)
            self.assertIn("control", (root / "out/summary.md").read_text())

    def test_main_replaces_stale_success_on_invalid_filter(self):
        with tempfile.TemporaryDirectory() as temp:
            root = self.fixture_root(temp)
            out = root / "out"
            out.mkdir()
            (out / "results.json").write_text('{"completed": true}')
            (out / "cargo.log").write_text("old successful run")
            with patch("run.ROOT", root), patch("run.command") as command:
                self.assertEqual(main(["--out", str(out), "--case", "typo"]), 2)
                command.assert_not_called()
            self.assertFalse(json.loads((out / "results.json").read_text())["completed"])
            self.assertEqual((out / "cargo.log").read_text(), "")
            self.assertIn("unknown case IDs", (out / "error.txt").read_text())

    @patch("run.load_json", return_value=[{"id": "other", "status": "passed"}])
    @patch("run.command")
    @patch("pathlib.Path.is_file", return_value=True)
    def test_core_report_cannot_omit_a_requested_case(self, is_file, command, load_json):
        command.return_value = SimpleNamespace(returncode=0, stdout="test passed")
        with self.assertRaisesRegex(ValueError, "incomplete or duplicate"):
            run_core([{"id": "control"}], [])

    def test_malformed_result_rows_are_rejected(self):
        for rows in [None, {}, [], [None], [{"id": "", "status": "passed"}],
                     [{"id": 7, "status": "passed"}], [{"id": "a", "status": "skipped"}],
                     [{"id": "a", "status": "passed"}, {"id": "a", "status": "passed"}]]:
            with self.assertRaises(ValueError):
                result_statuses(rows)

    def test_malformed_baseline_produces_an_incomplete_report(self):
        for invalid in ["[]", "null"]:
            with self.subTest(baseline=invalid), tempfile.TemporaryDirectory() as temp:
                root = self.fixture_root(temp)
                baseline = root / "baseline.json"
                baseline.write_text(invalid)
                with patch("run.ROOT", root), patch("run.command") as command:
                    self.assertEqual(main(["--out", str(root / "out"), "--baseline", str(baseline)]), 2)
                    command.assert_not_called()
                report = json.loads((root / "out/results.json").read_text())
                self.assertFalse(report["completed"])
                self.assertIn("report object", report["error"])


if __name__ == "__main__":
    unittest.main()
