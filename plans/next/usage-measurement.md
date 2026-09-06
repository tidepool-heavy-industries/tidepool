# Focused recipe measurement

Source tested: `ba41e1cae4872305b612873ac748004b4c5d896e`.
Direct execution on 2026-09-06, inherited repository Nix shell (`IN_NIX_SHELL=impure`).
No Cargo/GHC build or real provider/service test executed. No dependency cut is
justified by these measurements.

## Executable identities

- Python 3.14.7: `/nix/store/b5bpi6zfajzzrwwpgba2q6li3nnya4bs-python3-3.14.7/bin/python3.14`,
  SHA256 `4b2d8a9f7fc956e7afacf31235bbc99f81a59f77932d8511dd6fb18e8b734a3d`.
- Bash 5.3.9: `/nix/store/f15k3dpilmiyv6zgpib289rnjykgr1r4-bash-5.3p9/bin/bash`,
  SHA256 `0cddca75b643facec5d89be07c6018e1a652604428a52a4900c9ed11dfd0ef3c`.
- Extractor and worker environment variables were unset in this child. The test
  intentionally supplies a fixture frontend; this says nothing about availability
  of the real toolchain through the existing resolver.

## Command and observations

Executed a Python stdin measurement harness with the following timed body (imports
`importlib.util`, `time`, and `unittest` occurred before timing):

```python
for iteration in (1, 2):
    start = time.monotonic()
    spec = importlib.util.spec_from_file_location(
        'extract_checks', 'scripts/tests/test_lib_extract.py')
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    suite = unittest.TestSuite([mod.ExtractHelpers(
        'test_owned_daemon_keeps_endpoint_through_worker_rotation')])
    setup = time.monotonic() - start
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    duration = time.monotonic() - start
```

| Iteration | Import + selection seconds | Test with fixtures/cleanup seconds | Selected/executed | Failures/errors/skips |
|---|---:|---:|---|---|
| 1 | 0.049980 | 1.144975 | 1/1 | 0/0/0 |
| 2 | 0.000248 | 1.029668 | 1/1 | 0/0/0 |

Both produced `Ran 1 test` and `OK`: `ExecutedPassed`, expectation passing.
This exercises the shell helper's owned-daemon persistent endpoint argument and
fixture cleanup, not a real worker rotation or hosted Haskell call. Test names
alone do not extend the assertion's coverage.

The same Python process ran both cases with fresh unittest instances and temporary
fixture directories. Imports and filesystem caches were not reset. These are
uncontrolled first/repeated observations, **not cold/warm performance claims**.
Interpreter/Nix startup is outside timing; fixture setup, mock process launch,
assertions and cleanup are inside the test interval and were not separately
instrumented. Cargo setup versus actual Rust test time remains unmeasured.

An initial identity-collection attempt raised `KeyError: TIDEPOOL_EXTRACT` before
selecting or executing any test. It is `DidNotExecute`, not a behavioral failure
or evidence that extractor-backed tests cannot run. The corrected harness used
optional environment lookups; no test was replayed after uncertain submission.

## Reviewed owning paths and limits

`justfile` routes test-lib/test-target through `scripts/battery.sh`; the latter
resolves extractor infrastructure and owns nextest/daemon cleanup. Its existing
failure artifacts include reproduction command and diagnostic logs, but successful
runs delete those wrapper artifacts. Guidance now explicitly calls for retaining
successful output separately and preserving pipeline status.

Only documentation changed in this candidate. No shipped prompt change is needed:
existing instructions already distinguish review, incorporation and cleanup.
No new runtime API, test launcher, dependency refactor or provider cost claim.
