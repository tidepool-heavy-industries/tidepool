# Root partial run-map integration

Accepted repaired candidate 2116c9c2a57a5cb7fcf470ec53fbfe236ab92957 and independent
review 769f8c85c8b4442103ec96d5488f09e2c347ded2. Root inspected the repair diff and
review evidence, integrated at 607dc219cddb42c2a73abb85a1a250720c1410a1, then directly
checked that revision in the inherited repository Nix shell:

- `just test-lib tidepool 'test(partial_map_)'`: ExecutedPassed, expected passing;
  5 selected/executed/passed, 98 skipped. Cargo test build 3m22s; test summary 0.007s.
  Per-run compile daemon teardown explicitly reported. Evidence:
  `/tmp/root-run-map-evidence/integrated-tests.log`.
- `cargo build -p tidepool --example run_map`: built, 13.57s reported dev build;
  `/tmp/root-run-map-evidence/example-build.log`.
- Direct example invocation on indexed historical run followed by Python JSON
  assertions: ExecutedPassed; 31 actor directories, 145 recorded events, usage
  explicitly Unknown. `/tmp/root-run-map-evidence/{map.json,summary.txt}`.
  Example SHA256: 18e050202fd7e1f6f81a04ed9b8a09e6854c4e943bc42cdcd779a5051f91878c.
- `cargo fmt -p tidepool --check`, `git diff --check`: passed; checkout clean.

This closes the partial inventory integration only. It does not establish full
run-map, custody repair, mounted service, billing completeness or comparative cost.
The retained lead received 607dc219 for incorporation and remaining product work;
its resulting incorporation/check report is pending.

Harness finding: root independently observed build time dominating these small
Rust tests, corroborating (not duplicating the timing of) the reviewer run.
Checkout/cache state and target build features differ; do not turn these timings
into controlled warm/cold or comparative performance claims.
