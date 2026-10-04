# Native Buck build migration

## Approved local workflow on swarm-01

The current accepted workflow uses the checkout's pinned Nix toolchain and
local Buck execution. NativeLink is running, but project closure, isolation and
cache gates remain pending. Keep Buck remote execution disabled with
`--local-only -c remote.enabled=false`; do not use the bootstrap platform file
to configure the project worker.

The checkout must be a provisioned Git checkout with `buck-out` bind-mounted to
its own directory on `/srv/build`. Ask the infrastructure owner to provision a
new mount for a new checkout; do not create a mount helper or replace an active
output. Before any Buck invocation, run `findmnt --mountpoint "$PWD/buck-out"`
and stop if it is not a mount. Run large work only in an admitted `build.slice`
unit; an administrator launches the approved job as the unprivileged `swarm`
user. The first Buck command, including metadata queries, must run inside that
unit so the Buck daemon shares its admission boundary.

Materialize the checkout's declared closure and configure from inside the
admitted build environment:

```sh
nix build .#buck-toolchain-closure --no-link
bash scripts/buck2-configure.sh
export PATH="$(awk -F ' = ' '$1 == "action_path" {print $2}' .buckconfig.local):/run/current-system/sw/bin"
buck2 build --local-only -c remote.enabled=false //tidepool/repr:tidepool_repr
```

Then retain the unit journal, exact command, source OID, selected and executed
test counts, exit status and log before reporting. Building a `rust_test` target
only links its executable; use an accepted focused runner to execute tests.
Do not claim remote qualification from local results.

## Historical remote-execution evidence

The following is retained as evidence for the exact 2026-09-30 revisions and
environment only. It does not qualify the current checkout, broader targets,
the current host admission path, or NativeLink's pending project isolation and
cache gates.

The focused run used Tidepool
`d9d82f46ff5f702a16257c2dd9e42fe745e27265` and harness
`814b1697226344e8fd16196666e41c184a73531d`, Rust 1.93.0 and GHC 9.12.2.
Rust compilation executed 36 remote actions; the unit test target compiled with
20 remote actions; the Haskell library executed two remote actions. Those builds
had zero local actions. The remote test profile executed the test binary on the
worker with `--remote-only`: all eight tests passed, and the worker journal
records the invocation. A second independent local checkout reused 36 actions
from cache. Changing its Rust source executed two new remote actions; restoring
that source and requesting the Rust and Haskell libraries reused all four
required actions from cache. This is historical evidence for those focused
targets and revisions only.

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

The native first-party graph covers the packages in
`scripts/buck2_cargo_features.py`. Local development/backend/test-support
features are retired: unsupported local features fail with package/root/edge
diagnostics rather than entering a global feature union. Dev-only support
remains on dev dependency edges. Source-only generation and target availability
are distinct from accepted execution evidence.

`build/native-targets.json` is generated from Cargo metadata and the reviewed
module ownership walk. Registered native wrappers own libtest discovery,
nonzero counts, bounded isolated execution and reports. Haskell target/module
rosters come from Cabal through `scripts/buck2-haskell-tests.py` and execute
shared Tasty trees. Test GHC packages are separate from the production worker's
package environment. Materialize `.#buck-test-ghc`,
`.#buck-haskell-test-closure` and `.#buck-jev-sources`, then configure `--tests`
for host tests. The test runtime closure retains both host and production GHC;
production worker compilation still uses its production toolchain alone.

Generated protocol/effect/fixture/corpus outputs are native provider artifacts.
Fixed prepared fixtures and the twelve corpus cohorts are runtime directories,
rather than Rust embedded blobs. Compile-fail controls are native positive/
negative compiler actions. `just fixtures-check [COHORT...]` selects the
existing cohort targets and their independent native GHC oracle; no updater,
ambient Cargo/Cabal runner or checked-in prepared inventory remains.

Native formatter/Clippy and optimizer-probe actions preserve the corresponding
source obligations. `just check` links native consumers without running tests;
`just verify` is broad integration, reserved for integration boundaries.
Empty Cargo harnesses remain compile-only and cannot count as passing tests.
See `bridge/haskell/tests.md` for current suite/resource interfaces.

Additional focused execution targets include:

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

First-party Rust and Haskell actions use the Buck `tidepool.profile` setting.
The checked-in default, `fast-dev`, preserves the existing per-crate Rust
optimization and leaves GHC at its normal development level. Use `debug` to
compile all first-party Rust at opt-level 0 and leave GHC unoptimized, or select
`production` for opt-level 3 Rust and `-O2` GHC. Rust test binaries follow the
selected profile so optimized host tests exercise the same first-party code as
the shipped host. Compiler flags are explicit action inputs: each profile has
its own Buck action key and reuse remains at the current crate/component action
boundaries. The pinned Nix compilers and third-party Rust optimization remain
unchanged.

For a native production runtime, build
`//build/package:native_runtime_bundle`. It owns the same-profile host/libtest,
frontend, worker, source stdlib, actors, assets and pinned runtime commands.
The build action records artifact hashes and a source contract from declared
package-owned inputs. `//build/rust:native_qualification_sources` materializes
those raw inputs at full repository-relative paths; generated products retain
their own producer relation. It does not claim a caller-supplied source OID.

Freeze through `build/package/qualification.py` after copying to the final
absolute path. The owner verifies the declared source contract against clean
tracked Git and recorded submodules, checks the derived workspace Gitlink,
and creates final-path compiler deployment authority. Its explicit descriptor
supplies all acceptance/run inputs; no arbitrary libtest/compiler paths or
independent Nix catalog substitution are accepted. See
`build/package/README.md` for exact freeze/run/exec commands.

`just exomonad-run BUNDLE DESCRIPTOR REPORT COMMAND...` and
`just exomonad-init BUNDLE DESCRIPTOR REPORT FLAGS...` use that frozen owner.
An unqualified raw bundle build, `--help`, or a catalog self-consistency check
does not establish the six production M2 gates or M1 browser acceptance.
Source declarations and their actual executed qualification remain separate
in the delivery evidence ledger.

The optional `matched_runtime_bundle` owns separate Nix catalog products.
Native source-backed configuration selects `.#buck-exomonad-runtime-tools`
independently of `--runtime-stdlib`; materialize that pinned closure before
bundle actions. Both modes preserve separately installed stock Codex and its
credential owner; neither builds or packages credentials.

The matched runtime bundle includes `share/exomonad/compiler-deployment.json`,
generated from that bundle's frontend, worker, and pinned GHC library directory
before any compiler endpoint is observed. The Nix `exomonad` package points
`TIDEPOOL_COMPILER_DEPLOYMENT` at the same manifest packaged with
`tidepool-extract`. Local `just` workflows generate
`target/compiler-deployment.json` from the selected frontend/worker pair and
export its absolute path; an explicit `TIDEPOOL_COMPILER_DEPLOYMENT` must name
an absolute path to a readable nonempty manifest and is left intact. Runtime
admission then checks configured producer and worker digests before binding,
with endpoint identity retained as a separate observation. The producer
identity binds the selected worker path, so Buck tests consume the canonical
`matched_runtime_bundle` output path. If that bundle is copied elsewhere, its
manifest must be regenerated there from the copied frontend, worker, and same
GHC library directory before use; the Buck bundle does not promise path-free
relocation.

The Nix `runtime-stdlib-sources` and `runtime-stdlib-products` packages produce
the immutable source and original product inputs for the shipped Prelude
cohort. `tidepool-module-package` is the producing CLI; the product derivation
runs `tidepool-module-package build --source-root SOURCE/lib --output-root OUT`
with the same configured compiler deployment as `exomonad`. Source and product
roots must be their final canonical Nix store paths. The version 1 catalog
does not support moving either tree or redirecting source files through aliases.
Development and authored source roots retain their existing capture policy.

`exomonad` selects the optional catalog with `TIDEPOOL_COMPILER_MODULES`.
The existing toolchain candidate owner validates its configured producer and
worker, complete source manifest, and per-module `owner.json`, `products.cbor`,
`skinny.hi`, `packages.cbor`, and `dependencies.json` files. Original product
bytes and module versions are preserved. The generated catalog's schema is
owned by `tidepool/toolchain/src/module_candidates/deployment.rs`; original
TPMOD payloads are admitted by the production execution-schema reader. The
producing CLI validates its completed catalog through that same owner. These
generated package resources are separate from the checked-in TPSTG fixtures.

The first producer requires an original product for every home source reached
from `Tidepool.Prelude`. A missing product refuses with its module and compiler
availability status. At runtime, current ordered source resolution, package
witnesses, GHC interface checks, and candidate closure admission still determine
reuse. A valid candidate shadowed by authored code can fall back to fresh
compilation; invalid configured catalogs refuse at package source or candidate
selection. Sealed exact contexts use their own admitted artifacts, and the
package producer bypasses configured candidate input. The first package builds the
cohort in one action and exposes independent module files. It does not claim
independent build actions per module. Buck's copying `nix_directory` rule cannot
relocate this catalog; consuming the original store roots requires declared
resources that preserve those roots.

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
Generated worker artifacts, test fixtures, browser assets, and web `dist`
outputs are separate declared actions with explicit source, resource and
toolchain inputs. Remote execution remains disabled until its independent
closure, isolation, reuse, and cancellation checks pass.

## Daemon lifetime and cache reuse

Keep this checkout's Buck daemon inside the admitted `build.slice`. The first
metadata query starts it, so launch that query from the same admitted service
that owns the checkout's builds. An administrator can use this shape for an
approved build unit; substitute the assigned unit name and checkout:

```sh
sudo systemd-run --unit=tidepool-buck-daemon --collect \
  --uid=swarm --gid=swarm --slice=build.slice \
  --working-directory="$PWD" --setenv=HOME=/srv/swarm/home \
  /run/current-system/sw/bin/bash -lc \
  'bash scripts/buck2-run.sh targets -c remote.enabled=false //bridge/atomic-write:'
```

Inspect the service cgroup and daemon PID before builds. Run build and test
clients in the same admitted slice so they can reuse the daemon. Do not restart
a daemon with active work; coordinate any restart of an idle daemon owned by
this checkout. Local execution remains required while the NativeLink project
closure, isolation and cache gates are pending.

Codex is deprecated and excluded from this migration. Its existing build path
is retained separately. Toolchain-only Nix evaluation uses the repository's
committed toolchain source so Buck setup does not fetch or build Codex.
