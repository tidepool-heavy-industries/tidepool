# Native Buck build migration

## Author locally, execute on swarm-01

The isolated project worker advertises its own platform identity. Copy that
identity through authenticated SSH; the bootstrap worker's
`/etc/swarm-build/platform` does not include the project's Rust/GHC closure.
Keep the forwarding command running in another terminal:

```sh
ssh -N -o ExitOnForwardFailure=yes \
  -L 127.0.0.1:50071:127.0.0.1:50051 swarm-01
ssh swarm-01 cat /home/inanna/remote-buck-infra/worker-result/platform \
  > /tmp/tidepool-buck-platform
```

Each local checkout needs its own output directory and real bind mount. On
Linux, an unprivileged mount namespace can supply it without changing the
host's mounts. Run from the checkout root:

```sh
mkdir -p buck-out "$HOME/.cache/tidepool/buck-local"
export TIDEPOOL_BUCK_OUTPUT_DIRECTORY="$HOME/.cache/tidepool/buck-local"
unshare --user --map-root-user --mount bash -c '
  set -euo pipefail
  mount --bind "$TIDEPOOL_BUCK_OUTPUT_DIRECTORY" buck-out
  export TIDEPOOL_BUCK_REMOTE=true
  export TIDEPOOL_BUCK_PLATFORM_FILE=/tmp/tidepool-buck-platform
  export TIDEPOOL_BUCK_REMOTE_ADDRESS=grpc://127.0.0.1:50071
  bash scripts/buck2-configure.sh
  bash scripts/buck2-run.sh build //bridge/atomic-write:tidepool_atomic_write
  bash scripts/buck2-run.sh test --remote-only //bridge/atomic-write:tidepool_atomic_write_unit_tests
  bash scripts/buck2-run.sh build //bridge/haskell:assignment_internal
'
```

Choose a distinct output directory for every checkout. Later invocations still
enter a mount namespace and mount the same owned directory before calling
`buck2-run.sh`. The worker closure and local toolchain inputs must match.
The client accepts a loopback endpoint and a platform file; both remain local
configuration and are not committed. Editing source locally uploads changed
inputs through Buck's CAS protocol.

The focused readiness run on 2026-09-30 used Tidepool
`d9d82f46ff5f702a16257c2dd9e42fe745e27265` and harness
`814b1697226344e8fd16196666e41c184a73531d`, Rust 1.93.0 and GHC 9.12.2.
Rust compilation executed 36 remote actions; the unit test target compiled with
20 remote actions; the Haskell library executed two remote actions. All three
builds had zero local actions. The default remote test profile then executed
the test binary on the worker with `--remote-only`: all eight tests passed,
and the worker journal records the binary's invocation. A second independent local checkout reused 36
actions from cache. Changing its Rust source executed two new remote actions;
restoring that source and requesting the Rust and Haskell libraries reused all
four required actions from cache. This qualifies these focused targets; broader
extractor, browser, runtime and cancellation gates remain separate.

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

For an unpublished review join, `buck2-reindeer.sh --local-harness-source
--harness-source-override /nix/store/...-source` accepts an exported immutable
source only when its NAR hash matches the canonical `harnessWeb` lock. The
configured `matched_harness_source` must still equal the existing Nix producer's
output for those bytes. This override leaves canonical dependency URLs and the
default Git-fetch generation unchanged; it does not publish the reviewed commit.

The initial accepted native test slice covers `tidepool-atomic-write`,
`tidepool-repr`, and `tidepool-heap`:

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

Additional focused native gates are recorded in
[`engine-harness-completion-evidence.md`](../plans/engine-harness-completion-evidence.md).
Current execution targets include:

| Target | Execution boundary |
| --- | --- |
| `//tidepool/runtime:runtime_admission_tests` | Three counted admission tests; no compiler worker |
| `//bridge/facade:facade_process_tests` | Six counted framing and process cleanup tests |
| `//bridge/facade:facade_host_tests` | Three real host tests; compiler and browser assets |
| `//bridge/facade:facade_late_output_test` | One retained-output/compaction test |
| `//bridge/facade:facade_browser_test` | One explicitly selected ignored Chromium journey |
| `//bridge/haskell:planned_declaration` | Original declaration/interface authority suite |
| `//bridge/haskell:source_boot_product_reuse` | SOURCE/boot product reuse and invalidation suite |
| `//build/package:matched_runtime_smoke` | Assembled CLI, worker protocol and asset checks |
| `//build/package/worker-compile-smoke:packaged_worker_compile` | Packaged worker compiles a declared Haskell fixture |

Target availability is not a passing acceptance result. In particular, the
real host/browser gates remain subject to the evidence ledger. Runtime and
facade execution groups share their package's linked `*_unit_tests` binary;
that binary target is build-only. The counted wrappers refuse missing names,
zero matches and wrong counts, run each selected case in its own process, and
own timeout/cancellation cleanup. Run `buck2 test` on the execution targets.
`//bridge/facade:tidepool_unit_tests_all` is the explicit broad nonignored action;
do not use it for routine spot checks.

Haskell test fixture trees are declared filegroups copied into private scratch
before execution. Changing a fixture does not relink its test binary. Compiler,
browser and process resources belong to the groups that use them, so a process
framing test does not build Chromium or start GHC.

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
