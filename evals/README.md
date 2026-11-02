# Project evals

These are deterministic scenario evaluations of the Rust service's observable behavior. This project has no model inference or prompts in its request path, so there are no model scores or paid model calls. The organization takes inspiration from [T3 Code's eval guidance](https://github.com/pingdotgg/t3code/tree/main/evals): versioned cases, source attribution, positive controls, real implementation imports, reports, and baseline comparisons.

| Suite | What it evaluates | Execution |
| --- | --- | --- |
| `core` | Shared request and URL admission, active yt-dlp metadata normalization, privacy, malformed output and upstream failure classification | `tests/evals.rs` reads `cases.json`, imports production Rust functions and invokes the real adapter with a synthetic executable |
| `remote` | Cloudflare worker redirect boundaries, truncated/oversized streaming, cancellation cleanup, suspended VM readiness and idle lifecycle | `remote-cases.json` selects focused `eval_*` cases beside the real remote adapter's loopback mock tests |

They do not call YouTube, GCP, Cloudflare, or a live controller. They do not establish deployed availability, WARP health, IAM correctness, playable output, or an SLO. Existing media/API tests and opt-in live smoke checks serve those purposes. Legacy native player parsing retains its own regression tests; it is not presented as the active metadata adapter here.

## Run and compare

Requirements: the pinned Rust toolchain, Python 3.10+, and a Unix shell (Linux/macOS). Cargo may fetch missing dependencies from its normal registry; scenario execution itself uses only local files, synthetic subprocesses and loopback sockets. FFmpeg/yt-dlp/Node binaries and credentials are not needed for these suites.

From the repository root:

```bash
python3 -m unittest discover -s evals -p 'test_*.py'
python3 evals/run.py --out work/evals/baseline
python3 evals/run.py --out work/evals/current --baseline work/evals/baseline/results.json
python3 evals/run.py --suite core --case metadata-adaptive-private --out work/evals/one-case
python3 evals/run.py --suite remote --out work/evals/remote
```

`--case` accepts an exact ID from either case file and can be repeated. A baseline must have the same selected IDs and unchanged corpus digest. Use separate output directories to retain the baseline. A changed corpus requires a new baseline; do not silently compare different cases. All scenarios must pass even when a baseline already contained failures.

Each run writes `results.json` (case results, provenance, corpus digest and regressions), `summary.md`, and `cargo.log`. Exit codes: `0` all passed; `1` one or more scenario failures; `2` invalid input, missing tests, compilation failure, or incomplete execution. An incomplete report has `completed: false` and `error.txt`; it is not a baseline. Cargo itself must find and execute each requested test, so a renamed or misspelled test cannot yield a zero-test pass. Reports contain synthetic data only; do not commit run outputs.

For ordinary Rust validation, `cargo test --locked --test evals` runs the core corpus and grader guards without writing a report. The report runner runs each remote case by its exact test name, and the regular library suite covers those cases as well.

## Add a case

1. Reproduce a concrete failure or protect an important supported workflow. Add a case to `cases.json` with a stable `id`, a `source` explaining its contract and origin, a scenario, and an expected outcome. Cases are synthetic; never capture real tokens, signed media URLs, account details or upstream HTML.
2. Use `url` for shared URL admission, `request` for JSON decoding plus request/URL validation, or `metadata` for active yt-dlp subprocess output. Successful cases specify a nonempty `contains` object. Objects are compared by specified fields; arrays must have the same length and ordering. Rejections specify the exact public error code. The metadata evaluator independently checks canonical subprocess arguments and omission of signed URLs from public metadata/errors.
3. For remote lifecycle or streaming bugs, add a focused `eval_*` test in the remote module using its existing loopback mock, then register the exact full name and source in `remote-cases.json`. Exercise production behavior rather than copying the algorithm into an evaluator.
4. Run the affected cases before the fix and confirm the intended failure, then verify the fix. Keep a supported positive control so a fix cannot pass by rejecting everything. Do not loosen expectations to accommodate a regression.

The corpus is a maintained contract set, not a claim to exhaustively grade the service. Extend an existing case where it captures the same failure; use a new case when the failure has a different cause or expected result. No new dependencies or external evaluator service are required.
