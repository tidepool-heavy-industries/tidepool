# Shared worktree-agent contract

The parcel prompt defines the task. This file defines how to execute and
report it. Follow both; the narrower parcel constraint wins when they differ.

## Stay inside the parcel

- Rebase onto the requested local dependency commits before editing.
- Read the repository and crate guidance named by the parcel prompt.
- Preserve unrelated work. Do not commit `prompt.md.tmp`.
- If a focused test exposes a real defect in the owning mechanism, fix that
  mechanism narrowly. Do not grow a second implementation around it.

Before adding an API or abstraction, search its intended production callers.
A helper used only by its new test is not evidence that the abstraction belongs
in production. Prefer one owning mechanism whose public entry point enforces
its invariants; callers should not need to remember a separate pre-check.

## Focused verification is still complete verification

The ban on broad batteries is a resource constraint, not permission to leave a
changed target uncompiled.

Before committing:

1. List every changed file and map it to its owning build or test target. If a
   public type, serialized shape, generated contract, or shared command changes,
   search the whole workspace for consumers and fixtures; the defining crate is
   not the complete target map.
2. Compile every changed or directly affected build/test target, even when
   executing it would be too expensive. For Rust integration tests, use the
   owning compile-only recipe when execution is excluded. In the Codex fork,
   `cargo nextest list -p PACKAGE --test TARGET` builds/enumerates without running;
   its `just test` wrapper adds `--no-fail-fast`, which conflicts with `--no-run`. Compile one named downstream target rather than
   substituting an unrelated workspace-wide check.
3. Run the smallest exact tests that exercise each changed behavior and each
   important failure/recovery branch. A unit test in a neighboring target does
   not prove that a modified integration target compiles.
4. For extractor-backed behavior, first discover the repository's current
   toolchain environment and run one targeted test when resource limits permit.
   Do not silently treat a missing ambient variable as proof that the test is
   unavailable.
5. Reuse existing exact-source checks unless source changes invalidate them.
   Integrated revisions need checks of the changed joins, not automatic replay of
   every child check. Investigate failures with the smallest reproducer; after a
   repair rerun that case and affected consumers rather than the whole suite.
6. Run formatting or the narrow crate check needed for warnings, plus
   `git diff --check`.

Never run a prohibited workspace battery, broad suite, background test, or
parallel test command. If a necessary focused command unexpectedly fans out or
the host is saturated, stop it and report the exact unverified target.

Use precise result words:

- **passed**: the named test executed and passed;
- **compiled, not executed**: the target built via `--no-run` or equivalent;
- **skipped**: name the concrete blocker and the command that remains;
- **not tested**: only when no meaningful focused check exists, with a reason.

Never summarize a neighboring target's success as though it covered all files
you changed.

For diagnostic output from a passing focused test, use the existing nextest
setting rather than changing the test to panic:

```sh
NEXTEST_SUCCESS_OUTPUT=immediate just test-target PACKAGE TARGET 'test(NAME)'
```

This preserves output capture/isolation and the test's actual pass/fail result.
A baseline regression that executes and fails as expected is **executed, failed
as expected**, not passed, compiled-only, skipped, or blocked. Report observed
outcome separately from whether it matches the baseline expectation.

## Review before delivery

- Read the final diff as a reviewer, not merely as its author.
- Search for stale callers, duplicated policy, unused public surface, rendered
  string control flow, and comments that describe the old morphology.
- Check the negative path and recovery/cleanup behavior, not only the happy
  path.
- Confirm the worktree is clean after committing and report the commit hash,
  exact commands, and exact results.

## Focused recipes and retained evidence

Select the owning target and exact nextest test name, not only a substring:

```sh
just test-lib tidepool-agent 'test(=backend::codex::active_update::tests::only_the_exact_persisted_user_message_confirms_presentation)'
# For an integration test, substitute the actual package, file target and full name:
just test-target PACKAGE TARGET 'test(=FULL_TEST_NAME)'
```

These recipes enter the repository Nix environment and call `scripts/battery.sh`.
Even a Rust-only selection resolves extractor infrastructure and may start a
compile daemon; do not count total wall time as test-body time. The wrapper owns
signal handling and daemon teardown. Do not introduce an alternative launcher.
For the shell helper's mocked process boundary alone, a single existing test is:

```sh
bash scripts/dev-shell.sh python3 scripts/tests/test_lib_extract.py ExtractHelpers.test_owned_daemon_keeps_endpoint_through_worker_rotation -v
```

This executes a fixture frontend, not real GHC extraction or mounted service
acceptance. Use `bash scripts/dev-shell.sh COMMAND...` for direct tools. It selects committed
flake inputs while commands stay in the current checkout. Never use `nix develop
path:.` on a mounted workspace: that imports the warm build tree into the Nix store.
`IN_NIX_SHELL` alone does not identify the correct compiler environment. Commit
intentional toolchain changes or select a revision-pinned `TIDEPOOL_DEV_FLAKE`. A missing inherited extractor variable is not proof that the real
extractor is unavailable: the owning resolver discovers/builds it.

Retain the command, source revision, resolved executable paths (prefer hashes),
selected/executed/skipped counts, and observed result independently of expected
result. Check nonzero execution; compilation, a listing, unknown counts or a
zero-selection exit cannot close a behavior obligation. `battery.sh` retains
failure artifacts under `target/tidepool-test-runs` (override with
`TIDEPOOL_TEST_ARTIFACT_ROOT`) but deletes its artifacts after success. Capture
successful output separately if it is delivery evidence; when piping through
`tee`, enable `pipefail` so capture cannot mask failure.

For timing, name measured boundaries: environment/build/daemon startup, test
execution (including fixtures if inseparable), and cleanup. Record unknown
portions instead of subtracting an unrelated command. State process reuse and
cache conditions; a second invocation does not establish a controlled warm-cache
benchmark. Do not clear shared caches to manufacture a cold run. No inference
about serial-versus-tree cost follows from a focused recipe measurement.

Reuse the same Cargo target directory for compatible validation commands; do not
create a fresh target per test or diagnostic attempt. Retire obsolete validation
caches after recording results. Selected launch binaries and recovery records
belong to the run directory, not disposable build output.
