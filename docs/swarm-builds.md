# Native Buck build migration

The Buck graph is being introduced alongside the existing `just` workflows.
Cargo metadata and `Cargo.lock` remain authoritative for Rust package versions
and dependency edges. `scripts/buck2-first-party.py --package NAME` generates a
bounded set of native Rust targets; `scripts/buck2-reindeer.sh` generates the
locked third-party crate graph. Their `--check` modes fail when the committed
graph is stale. Do not wrap Cargo or Cabal workspace builds in opaque Buck
`genrule` targets.

Regenerate or verify the initial slice in the pinned development shell:

```sh
bash scripts/dev-shell.sh scripts/buck2-first-party.py \
  --package tidepool-atomic-write --package tidepool-repr
bash scripts/dev-shell.sh scripts/buck2-reindeer.sh
```

Add `--check` to either command to verify generated inputs without retaining
regenerated output.

The accepted native slice is `tidepool-atomic-write`, `tidepool-repr`, and
`tidepool-heap`:

- `//bridge/atomic-write:tidepool_atomic_write_unit_tests`
- `//bridge/atomic-write:strict_directory`
- `//tidepool/repr:tidepool_repr_unit_tests`
- `//tidepool/repr:repr`
- `//tidepool/heap:tidepool_heap_unit_tests`
- `//tidepool/heap:gc_unit`
- `//tidepool/heap:raw_scan_validation`

These are native Rust rules with crate/test sources and compile-time fixtures
mapped to repository-relative paths through `rust_filegroup`. Fixture owners
export individual files. Fault-injection shared libraries are separate native C
actions supplied through the test environment, without becoming Rust link inputs.
A Buck test result is evidence only for its selected target; all
other packages retain their existing `just` checks until migrated and accepted.

## Toolchain and output setup

The flake pins Buck2, Reindeer, Rust, GHC, C/C++, and the action support tools.
Materialize the declared action closure before configuring the checkout:

```sh
bash scripts/dev-shell.sh bash -c 'nix build --no-link "${TIDEPOOL_DEV_SHELL%#*}#buck-toolchain-closure"'
bash scripts/buck2-configure.sh
```

`buck-out` must be a bind mount of a per-checkout directory on `/srv/build`; a
symlink is not supported. Check `findmnt --mountpoint "$PWD/buck-out"` before
any Buck invocation. Do not run Buck metadata queries or builds from an
unadmitted agent shell. For initial validation use the admitted build slice and
keep execution local while NativeLink gates remain pending:

```sh
bash scripts/buck2-run.sh build --local-only -c remote.enabled=false //tidepool/repr:tidepool_repr
bash scripts/buck2-run.sh test --print-passing-details --local-only -c remote.enabled=false \
  //bridge/atomic-write:tidepool_atomic_write_unit_tests \
  //bridge/atomic-write:strict_directory \
  //tidepool/repr:tidepool_repr_unit_tests \
  //tidepool/repr:repr
```

Report the source OID, exact command, selected/executed test count, exit status,
and retained log. Building a test binary is not running it. Re-run once to
measure reuse, distinguishing a warm dependency graph from action-cache hits,
then change one selected source or fixture and
verify only its affected dependency closure rebuilds.

The exact Prelude bundled with Buck snapshot `20260926-200119` uses one
`haskell_compile_*` action for all sources of each target/link style. Its
`haskell/compile.bzl` gathers the target's Haskell sources and hidden boot inputs,
then submits a single compiler action (inspected locally after materialization;
SHA-256 `04bf88781d70b603d4ca7679d11efb72c8b35fab965c82103f0f5186559e2f27`).
Per-module Haskell cache granularity is therefore still open. Model import,
Template Haskell and boot/SCC dependencies explicitly before replacing that
boundary; splitting target names alone does not establish independent caching.
Generated worker artifacts, test fixtures, embedded browser assets, and web
`dist` outputs must remain separate declared actions with explicit source,
resource, and toolchain inputs before those surfaces move from their existing
Cabal/Nix workflows. Remote execution remains disabled until its independent
closure, isolation, reuse, and cancellation checks pass.

## Daemon lifetime and cache reuse

Keep this checkout's Buck daemon in a persistent user service within the admitted
slice. A temporary build service stops its daemon when the client exits, losing
the in-memory dependency graph. After confirming no daemon for this checkout is
already owned elsewhere, launch the first metadata query as follows:

```sh
systemd-run --user --collect --property=RemainAfterExit=yes \
  --unit=tidepool-buck-daemon --slice=tidepool-completion-build.slice \
  --working-directory="$PWD" \
  /run/current-system/sw/bin/bash scripts/buck2-run.sh \
  targets -c remote.enabled=false //bridge/atomic-write:
```

Inspect the service cgroup and daemon PID before builds. Run build/test clients
in the same admitted slice; they reuse the daemon. Do not restart a daemon with
active work. The service is user-owned and does not change host configuration.
Local execution is the default even if the host has a NativeLink endpoint;
remote use requires explicit configuration after separate acceptance.

Codex is deprecated and excluded from this migration. Its existing build path
is retained separately. Toolchain-only Nix evaluation uses the repository's
committed toolchain source so Buck setup does not fetch or build Codex.
