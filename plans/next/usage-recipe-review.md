# Independent focused-recipe review

Reviewed candidate: `7567241016e3da55b3d7781b006dc7e1149446ee`.
Rerun source: integrated seed `1d17100cfab7c535b310cde34952ed27da7d76f4`;
the two candidate documentation files are unchanged at that seed.

Source inspection traced the exact nextest and Python names to their tests,
`justfile` target dispatch to `scripts/battery.sh`, and artifact retention to
`prepare_battery_artifacts` / `finalize_battery_artifacts` in `lib-extract.sh`.
The guidance accurately distinguishes mocked fixtures from real extraction and
successful output capture from wrapper failure artifacts. No production edits.

Independent direct command in inherited repository Nix shell (`IN_NIX_SHELL=impure`):

```
python3 scripts/tests/test_lib_extract.py ExtractHelpers.test_owned_daemon_keeps_endpoint_through_worker_rotation -v
git diff --check
```

Result: selected 1, executed 1, failures/errors/skips 0; `Ran 1 test in 1.029s`,
`OK`, exit 0. Diff check also exited 0. Expectation passing, ExecutedPassed.
This tests mocked frontend launch arguments and fixture cleanup, not actual
worker rotation. The documented original two timings are attributed evidence;
this rerun independently verifies the focused command, not their timing values.

No repair required for these documentation changes. The measurement is bounded
and honest but insufficient to resolve real Cargo/build/daemon setup versus Rust
test execution cost. That objective remains unverified; this review does not
recommend dependency surgery or claim a complete performance study. No broad
battery, real extractor compilation, provider, or mounted service test ran.
