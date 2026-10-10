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
and stop if it is not a mount. Develop and build as `inanna`. Use `swarm-build`
for the admitted `build.slice` scope; it enters the scope and runs the command
as `inanna`. The first Buck command, including metadata queries, must run
inside that scope so the Buck daemon shares its admission boundary.

Prepare the selected pinned outputs and configure inside the admitted build
environment. Configure realizes every selected output and registers a durable
Nix GC root before publishing `.buckconfig.local`:

```sh
swarm-build bash scripts/buck2-configure.sh --tests
swarm-build bash scripts/buck2-run.sh build --local-only -c remote.enabled=false \
  //tidepool/repr:tidepool_repr
```

Then retain the unit journal, exact command, source OID, selected and executed
test counts, exit status and log before reporting. Building a `rust_test` target
only links its executable; use an accepted focused runner to execute tests.
Do not claim remote qualification from local results.

### Test profiles and link inputs

Ordinary `just test-lib`, `test-target`, `test-bin`, `test-native`, `quick` and
`suite` commands explicitly select and report `fast-dev`. Use
`just --set native_profile production test-lib PACKAGE --exact FULL_NAME --expected-count 1`
for an optimized test, or pass `--profile production` before the command to
`scripts/native-workflow.py`. The same Just variable selects the profile for
`build`, `probe-opacity-check`, the script test recipes and `exomonad-build`;
`just --set native_profile production build LABEL` requests an optimized build.
Raw `buck2` commands select their configuration directly. Frozen qualification
still requires its explicit matching `--expect-profile`; a fast-dev result does
not qualify production bytes.

Fast-dev compiles test harnesses at O0, including engine unit tests that compile
their owning code again. The production libraries of `tidepool-codegen`,
`tidepool-repr` and `tidepool-heap` remain O3 and are reused by consumers. This
policy follows package identity and rule kind, so renaming a target cannot
change its optimization. Production selects O3 throughout and GHC `-O2`; debug
selects O0 throughout. Third-party Rust targets retain the pinned toolchain's
existing optimization policy.

Native linking retains the pinned GCC driver and selects the declared Nix LLD
output through its rooted binary directory. Configure refuses a missing LLD
executable and retains that output with the generation; ambient linkers do not
substitute for it. Reconfigure after this input changes.

Measure first profile materialization, cached compilation, and counted
execution separately. A cold fast-dev graph cannot establish steady-state
edit latency. Preserve production cache entries and frozen executables when
comparing profiles: a later build can replace a materialized executable at a
shared output path. Compiler-free focused cohorts should declare only their
actual runtime resources; full configure still materializes its wider selected
toolchain and is a separate setup cost.

### Worker builds and parallel acceptance

Workers run their own affected-target builds and focused tests through the
admitted commands. There is no build-owner approval queue. Compile test targets
as soon as a coherent change is ready, including test callers of changed APIs;
then execute the relevant counted tests. Source-only handoffs are explicitly
unqualified, not a default stage that waits for an entire wave to finish.

Before launching expensive work, inspect active jobs, their enclosing cgroups
and host headroom. Run independent work concurrently when measured peaks fit
both the enclosing limits and the host reserve. Coordinate only actual conflicts:
shared writable checkout or output state, configuration publication, overlapping
resource demand, or a frozen acceptance run whose inputs must remain unchanged.
Use provisioned checkouts and stable profiles to preserve Buck reuse. Do not
bypass admission or change host limits to obtain capacity.

For an edit/check loop, select the smallest affected test targets; building a
production library alone does not compile its test callers. Avoid adding
`native_runtime_bundle` to ordinary focused checks: it also assembles browser,
catalog and deployment inputs. Build and freeze the complete matched bundle
for release qualification. Record a concrete constraint when deferring a check;
an idle coordinator or an inherited single-build convention is not a constraint.

The existing system `build.slice` admits all 32 CPUs for the Buck daemon and its
descendants, with 80 GiB memory high, 96 GiB maximum and 2 GiB maximum swap. The existing user
`tidepool-completion-build.slice` admits all 32 CPUs, with 96 GiB memory high,
104 GiB maximum and 2 GiB maximum swap. These are separate enclosing budgets;
their sum is not additional physical memory. Verify the loaded properties
before using these host-specific values.

Keep Buck clients and the owned Buck daemon in `build.slice`. Launch frozen
acceptance through its canonical qualification owner with explicit `--jobs`,
`--delegated-service` and
`--service-slice tidepool-completion-build.slice`; see
`build/package/README.md`. The client may start through `swarm-build`, while
the existing isolated runner asks the user manager to admit each actual test
and its descendants into a separate service in the larger slice. This
preserves counted execution, per-case deadlines and complete process cleanup.
Verify the test process cgroup rather than inferring it from its client.

Start with three concurrent heavy hosted cases and schedule independent
lighter native work alongside them. Observed hosted cases have reached about
18 GiB per process tree; old live hosts also consume the user slice. Retain
each service's peak before it is collected, monitor both enclosing slices,
and keep roughly 20 GiB host memory available for transient growth, SSH and
the OS. Increase concurrency only when the measured peaks fit. All CPUs may
execute useful independent work; one serial compiler phase does not become
parallel merely by assigning it more CPUs.

Parallel semantic acceptance records shared-load wall times. Dedicated timing
comparisons retain their own admission conditions. Do not rerun already
qualified batteries solely to occupy idle CPUs, or change the bytes or
deadlines of an in-flight frozen cohort.

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

## Select targets for the claim

Choose the consumer and behavior the change must establish before choosing a
command. A target listing establishes availability, a linked executable
establishes compilation, and a counted run establishes only the selected
executed cases. Component checks, interactions across components, corpus
agreement with GHC, and frozen release qualification answer different questions.
Use the smallest target that can expose the changed behavior and its relevant
failure path; expand when dependencies or unresolved risk justify it.

Trace source registration as well as dependency edges. A passing package command
does not establish that a newly added module, fixture or integration target was
compiled or selected. Verify the production consumer, generated target inputs,
actual test discovery and nonzero execution count. Resolve uncertainty through
the owning metadata or runner rather than another broad build.

The `justfile` is a thin frontend to the declared native Buck graph.
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
rosters come from Cabal through `scripts/buck2-haskell-components.py` and execute
shared Tasty trees. Test GHC packages are separate from the production worker's
package environment. Configure with `--tests` to prepare and retain
`.#buck-test-ghc`, `.#buck-haskell-test-closure` and `.#buck-jev-sources`
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

Bundle process checks and executed acceptance belong to the central native
qualification owner; see `build/package/README.md` for its frozen descriptor and
report commands.

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
Native Rust wrappers declare single-path inputs in `resource_env`, separately
from flags, commands and path lists in `env`. The counted runner makes those
resource paths absolute against its Buck launch directory before discovery or
execution, preserving Buck symlink paths and refusing missing resources.

## Toolchain and output setup

The flake pins Buck2, Reindeer, Rust, GHC, C/C++, and the action support tools.
Configure prepares the exact outputs selected by its existing flags:

```sh
bash scripts/buck2-configure.sh
```

Each preparation writes an ignored checkout-owned directory under
`.buck2-toolchains/generations/`. Its `owner` records the checkout, Unix UID,
flake selection, selection mode, checkout-derived toolchain-input tree and flags;
`outputs.tsv` records each selected output's name,
flake reference, resolved store path and indirect GC-root link. `status` records
preparation success or failure, and `config` retains the published configuration.
The configuration comment and command output identify the exact generation.
Output realization, root/path validation and GHC libdir validation precede the
atomic configuration switch. A failed preparation preserves the prior config
and retains its partial roots and evidence for inspection.

Previous generations stay rooted by default. Before explicitly retiring one,
inspect its recorded paths against active Buck configurations, frozen bundles
and live processes. Do not delete the generation directory or its root links
while any consumer still needs them. Configuring a new generation does not
retire old roots, and checkout roots do not replace the independent retention
owned by a frozen native bundle's qualification descriptor. `nix build --no-link`
provides no durable retention and is not the checkout preparation workflow.
The pinned Buck CLI is a selected, rooted output alongside the action tools.
`scripts/buck2-run.sh` verifies the published generation, checkout and user,
unchanged checkout-derived toolchain-input tree, retained roots and declared
executable/PATH, then executes that CLI directly. Ordinary native commands do not
enter a Nix dev shell. A missing or stale generation fails with an explicit configure
instruction; it never selects an ambient Buck or prepares tools implicitly.
Configurations predating the rooted CLI require one admitted reconfiguration.
The default checkout-derived selection requires committed toolchain inputs;
unrelated source commits reuse the generation. An explicit immutable
`TIDEPOOL_DEV_FLAKE` selection owns its tools independently of local toolchain
edits. Resource admission remains with `swarm-build`, and the launcher preserves
that process's cgroup and exit status.

Optional host Haskell test outputs require `--tests`. Browser driver and Chromium
outputs require `--browser`, independently of `--tests`; native runtime bundles
also require it through their browser driver dependency. Select both for release
qualification or the explicit broad facade test group. Ordinary configuration leaves these optional
resources empty, and targets requiring them refuse the missing selection. The
embedded web asset tools remain ordinary inputs for native bundle assembly.
Catalog products retain their independent qualification owner.

The default development shell selects the production Haskell package environment.
`bash scripts/dev-shell.sh --tests COMMAND...` selects the additional Tasty providers and
QuickCheck packages from the same pinned compiler/package universe. Configure
enters the default shell and realizes its explicit `--tests` outputs separately.

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

Nix supplies pinned tools, source bytes and third-party browser assets. Native
project actions build the frontend, worker, module packager, host and catalog.
Configuration prepares and roots `.#buck-exomonad-runtime-tools`, including the
Python required by the native entrypoint, before bundle actions. Stock Codex and
its credentials retain their separately installed owner.

Catalog source retention, production and bundle qualification use the central
owner in `build/package/qualification.py`. Its native catalog source selection
contains the ordered effects, stdlib, actor and Jev roots plus the declared probe.
The producer admits original products from the matched compiler action; the
qualifier independently checks retained Nix source provenance and the frozen
bundle contract. Original source paths remain fixed while the complete product
container can move unchanged. See `build/package/README.md` for the exact source
retention, production, freeze and qualification commands. Compilation, diagnostic
inventory and self-consistency checks do not establish executed acceptance.

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
and retained log. Building a test binary is not running it. When investigating
build reuse, compare the unchanged-input repeat, a controlled source or fixture
change, and its restoration in an owned checkout. Distinguish a warm dependency
graph from action-cache hits and verify the affected dependency closure. These
controls test reuse and invalidation; routine source checks need no extra rebuild
once their evidence is sufficient.

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

## Diagnose the failed stage

Preserve the first failing action and complete stderr before retrying. Different
stages call for different evidence:

| Failure | First discriminator |
|---|---|
| Missing or stale toolchain generation | Compare the retained owner, selection, input tree, roots and status with this checkout; no project build has been established |
| Analysis, compilation or linking | Inspect the declared action and its inputs, source registration and first diagnostic; distinguish a missing input from a source or compiler defect |
| Empty or wrong test selection | Compare exact runner discovery with the expected module and target; a zero-test run is no behavioral evidence |
| Executed assertion or property failure | Retain the input, seed or history and matched artifact; minimize through the owning component before expanding the run |
| Timeout, process death or incomplete cleanup | Inspect retained execution and cleanup outcomes, descendant cgroups and memory events; preserve unknown outcomes rather than reporting a test verdict |

Before starting preparation or an expensive rebuild, inspect existing work and
resource headroom. Coordinate with another worker only when the operations
conflict. Share a completed compatible generation; serialize large closure
realization and avoid competing publication of checkout configuration.
Preserve failed generations and live artifacts for
their owners. Repeat a command or increase a deadline when it tests an explanation
or follows a repaired precondition; investigate cache behavior with controlled
inputs and owned artifacts while preserving shared caches.

## Daemon lifetime and cache reuse

Keep this checkout's Buck daemon inside the admitted `build.slice`. The first
metadata query starts it, so launch that query from an admitted scope
that owns the checkout's builds. From the provisioned checkout, use the
current admission launcher:

```sh
swarm-build bash scripts/buck2-run.sh targets -c remote.enabled=false \
  //bridge/atomic-write:
```

Inspect the scope cgroup and daemon PID before builds. Run build and test
clients in the same admitted slice so they can reuse the daemon. Do not restart
a daemon with active work; coordinate any restart of an idle daemon owned by
this checkout. Local execution remains required while the NativeLink project
closure, isolation and cache gates are pending.

Codex is deprecated and excluded from this migration. Its existing build path
is retained separately. Toolchain-only Nix evaluation uses the repository's
committed toolchain source so Buck setup does not fetch or build Codex.
