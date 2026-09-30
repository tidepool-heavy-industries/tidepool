# Engine completion and embedded harness delivery

Approved 2026-09-29. This sequences the accepted engine completion contract in
`engine-completion-next-wave.md` with M1-first harness delivery. Quality takes
precedence over compatibility and earlier implementation choices. Migrate
consumers instead of retaining weak internal boundaries. Persisted formats need
explicit migration/refusal. This document does not claim M2 acceptance.

## Approved completion sequence — 2026-09-29

This sequence supersedes conflicting earlier rollout and completion claims below.
The user approved implementation after a source-based completion review. The
finish line includes the new engine, the complete embedded application, native
Buck delivery, and a separately approved live browser trial. Embedded becomes
the default for new runs only after acceptance. Existing runs keep their recorded
backend. Stock Codex remains independently usable; Codex is excluded from Buck.

Starting revisions: main/Buck `b87f7a9b10`, joined engine `fb781d25a1`, M2 facade
`cacd0ccf9e`, harness library `3ff873a86acdb7e640c25392d1b88e7412758a60`.
The browser asset pin must match that library revision. Compiler, runtime and
child-attachment WIP was saved under
`target/completion-evidence/active-wip/20260930T000915Z` before implementation.

### Application order and owners

1. Root joins the accepted baselines and records each candidate's exact source
   and tests. Compiler fixes parsed-quasiquote dependency evidence and rejects
   incompatible certification request modes. Runtime closes real authored
   admission, using reserved slots and exact lexical baselines rather than
   allocator high-water equality. Harness closes actual host fork readiness and
   diagnoses the capture-test stack overflow, retaining operation-phase evidence.
2. Compiler issues exact multi-turn authored deltas and owned AcceptedJoin
   receipts: original export/record-parent identities, dictionary-to-class and
   axiom-to-family edges, selected instances and the full family consistency
   closure. Include all consulted package interfaces, including lazy orphan and
   family loads. Complete verified recursive/boot candidate reuse; unknown
   compile-time dependencies remain misses. Carry products into durable resident
   storage and close native demand/instance/reclamation acceptance.
3. Runtime pairs declaration, binding and native-instance visibility under the
   existing publication owner. Reserve durable sparse IDs before staging and
   burn failures. Stage outside checkout; revalidate the complete graph and
   admission snapshot; order cancellation against commit; rename the manifest
   before a preflighted infallible live swap. Stale successes/rejections restage
   without effects. Post-rename durability uncertainty stays published. Reopen
   exact Authored/Join graphs and lost-value tombstones without source/effect,
   heap, continuation or live-token replay. Version changed formats explicitly.
4. Actor execution records own private scope, immutable source/tool leases,
   continuations, controls, receipts and publication. Lifecycle stays actor-owned;
   structured actor turns remain non-reentrant. Reuse the existing scheduler,
   replay registry and checkout. Park compilation/external waits outside actor
   handlers, fence completions by execution and continuation generation, and
   scope cancellation/after-tool/request cleanup to the exact execution.
5. Harness adds explicit capture-backed unfold beside ordinary deferred unfold.
   Rust verifies every member's admitted checkpoint and selected scope, publishes
   and releases a ready group before resuming the cell, aborts prepublication
   failures, and retains successful children after later failure. An effectful
   awaitWatch subscribes atomically through the existing request/watch owner and
   parks without checkout; Await itself remains a dependency description.
   Complete addressed browser input/control and full model/Haskell actor tree
   projection, pending-call compaction/reconnect and honest host-loss behavior.
6. Build owners introduce an embedded-only product with legacy Codex dependencies
   disabled, including startup's executable discovery. Extend native Buck through
   compiler/runtime, actor/facade, matched harness/browser and package assembly.
   Keep generation, fixtures, compilation, linking and execution separately
   keyed. Use standard native Haskell component granularity; custom module/SCC
   rules are a measured follow-up. Nix supplies toolchains; the resident compiler
   stays a runtime service. Keep the credential-file bridge explicit.

### Required completion evidence

- Actual resident cache miss/hit, dependency/boot/package invalidation,
  source-hidden consumers, instance/family isolation, demand omission, rollback,
  inherited versus fresh instances, and final-owner reclamation.
- A parks, B publishes, A resumes without erasing B; completion-order shadowing;
  old captures retain meaning; invalid joins expose no partial delta.
- Two parked cells plus progressing third/control, both cancellation/commit
  orders, stale completions, pre/post-rename faults, recovery and retained
  uncertain cleanup.
- An unfinished real hosted cell captures, launches two children, receives typed
  results, then fails; children and capture remain usable. Cover release races,
  mixed-context rejection, partial launch failure and real host-loop readiness.
- Actual application/browser journey: raw Haskell, recursive tree, typed routing,
  reload, raw/structured pending compaction, cancellation, authenticated reconnect
  and explicit unavailable live state after host loss.
- Required repository verification constituents at the final joined revision,
  matching harness/web tests and native Buck packaged startup. Retain exact
  nonzero executed counts, source/artifact hashes and logs. Demonstrate controlled
  input invalidation and distinguish warm daemon reuse from action-cache hits.
- Prepare exact task, model/round limits and budget for separate live approval.
  The browser Sol/Luna trial must produce reviewed integrated work, checks,
  retained trace and resource disposition. Only then switch new-run defaults.

Root integrates shared interfaces and also implements a concrete parcel. Up to
8 workers own disjoint compiler, runtime, actor, harness, browser, build and review
work; Sol owns cross-component mechanisms and Luna bounded components. Preserve
WIP and daemons, use admitted concurrent builds, and keep remote Buck disabled
until its separate infrastructure gates pass. Publish dependencies before root
when credentials are available; otherwise retain verified bundles.

## Starting evidence

| Owner | Checkpoint | Evidence / limit |
| --- | --- | --- |
| Joined Tidepool | `5bf38a34e2f89666c732f12bf3807b67825ffb0b` | Earlier joined native/host 11/11 at `bef801b6c`; later exact execution fencing integrated. |
| Compiler | `26e17afb6f13a370460ba8e0f14e48ac700d8ced` | Real module hit/miss and source-hidden proof pass; resident certification and recursive reuse unfinished. |
| Native | `1ddb871d88b3f956d4c65bd85c36554981331305` | Exact-owner runtime 2/2, demand 2/2; production resident consumer unfinished. |
| Runtime | `c8d13763491952eb178dbed7352d282a5550c7f1` | Wrapper 3/3, recovery codec 7/7; publication/concurrent execution/v2 adoption unfinished. |
| Declaration validator | `283ef49cc2dfffd2ac5e20b91d1245ab644029e0` | Home isolation and actual fresh worker consumers pass; package isolation unproved. |
| M1 | `3606f16096468e7ee4a17b17401b1a9b776ce67c` | Real-cell and pending-call/compaction/cancellation checks; successful complete application-host test still missing. |
| Packaging | `7b4525710719df0018b8c5c9a539f1aaea69dd91` | Exact-source package built with immutable browser assets; not final joined acceptance. |
| Harness | `c485edb9b697ffc671b22c9ef25a73fc84763d76` | Existing adapter pin, ten commits after `f495e93`; identity/schema-v5/admission/cancellation changes must be verified together. |

Compiler, declaration and runtime WIP is retained in the existing isolated
worktrees. Exact tracked/untracked inventories and patches are retained under
the dated completion evidence directory before integration. Preserve worker
branches and source evidence; integrate reviewed commits without duplicating
equivalent cherry-picks. Earlier standalone harness test totals do not establish
acceptance of the ten later commits.

## Ordered parcels

1. **Harness contract / M1 baseline.** Join reviewed host tests and packaging
   onto the sequential engine baseline. Start harness changes from c485edb9.
   Keep qualified OperationId and schema-v5 migration together. Remove embedded
   wait_agent injection/exemption; preserve standalone behavior and existing
   final-with-pending internal wait, nonfinal async progression and durable wake.
   Pin any repaired library and assets to the same reviewed revision.
2. **Full application-host M1.** Facade owns a private deterministic transport
   test seam. Exercise readiness, authenticated browser command, actual resident
   Haskell, retained output, reconnect/history and identical command retry without
   execution replay, then retirement. Derive manifest and dispatcher from the
   same request-pinned endpoint; remove stale installation declaration caching.
   Test reload, failed reload, real cancellation, input inclusion and host loss.
   M1 is explicitly sequential; no child/capture or atomic-cell claims.
3. **Certified compiler/native production.** Worker/toolchain own exact products,
   source/boot/interface/package evidence and explicit certification outcomes.
   Every authored/Join request receives exact retained versus lexical scope refs,
   selected inventories and sealed direct package roots. Missing evidence is not
   empty evidence. Prove recursive reuse with boot and external dependency checks.
   Full package isolation includes forced lazy metadata loads; if an interface
   callback cannot enforce it, use a pinned pre-publication loader hook rather
   than weakening isolation. Check the full retained family closure plus new
   imports/local families on every authored compilation and Join.
   Runtime resolves only demanded retained imports against the admitted lexical
   scope. Preserve full source owner/ordinal through sealing and inherited group
   reuse, including late-demanded sibling binders. Compile target/new groups off
   checkout; install, scope-register and publish metadata atomically. Bootstrap,
   split preparation and stale retries use the same path. Required certification
   failure may retry fresh compilation once, never downgrade to legacy install.
4. **Private execution/publication/recovery.** Execution records own private
   scope, final writes, continuation/control/receipts and admitted source/tools.
   Complete exact Join adoption and paired visibility including hidden-host
   policy/epochs. Reserve durable sparse IDs and burn failures. Stage proof,
   artifacts and manifest outside checkout; restage stale successes/rejections
   without effects. Manifest rename is commit authority, followed by infallible
   visibility swap. Post-rename fsync failure is published/durability-unconfirmed.
   Wire v2 attach/publication/import restoration/tombstones and explicit per-actor
   durable manifests. No heap/token/effect replay. Enable execution interleaving
   only after publication/cancellation and multi-execution tests pass.
5. **Captures and embedded children.** Carry exact origin OperationId through a
   narrow host capability into capture. Existing ForkGroupRegistry retains the
   opaque harness checkpoint; no second registry/supervisor. Reserve actor state,
   retain exact scope/admitted source, commit Store capture before returning the
   token, then publish its combined lease before dispatching the next effect.
   Failed delivery drops capability and retains exact cleanup custody; historical
   rows are unavailable, not resurrectable state. No DB transaction spans Haskell.
   Existing actor installation attaches children via Conversation::from_checkpoint.
   Distinguish transcript origin, creator and supervisor. One shared Store,
   scheduler/server per run, one model-round owner per conversation. Released
   tokens reject new admissions; existing children and successful captures survive
   issuer failure. Browser includes actual Haskell-only workflow actors.

## Native Buck migration

Added 2026-09-29 alongside implementation. Make Buck the native project build
and test graph wherever viable, not a wrapper around whole Cargo/Cabal builds.
Use standard rules and toolchains, explicit dependency edges and declared inputs;
keep generation, compilation, test execution and browser assets separate cacheable
actions. Preserve existing authoritative checks until equivalent executed Buck
coverage is accepted. Nix continues to supply pinned toolchain closures; the
resident Haskell compiler remains a runtime service.

Port build metadata from `build/buck2-swarm` onto the accepted engine source,
without importing its old source ancestry or replacing current flake outputs.
Begin with atomic-write and repr, then expand through native engine/runtime,
extractor, facade and matching harness/browser actions. Regenerate and validate
Rust dependency metadata against current manifests and lockfiles. Inspect actual
Prelude Haskell actions before choosing module target boundaries: cache
granularity is determined by actions and their inputs, not target count alone.
Avoid opaque workspace-wide actions where native incremental rules are viable.

Run Buck from an existing checkout with a real `buck-out` bind mount, with its
daemon and children admitted to the completion slice. Preserve default isolation
for cache reuse. Use `--local-only -c remote.enabled=false` until remote closure,
isolation and cache acceptance is separately recorded. No remote-cache claims
follow from local hits.

For each migrated boundary, retain a cold build, unchanged warm build, executed
test counts, and a controlled source/fixture invalidation check showing only the
affected dependency closure reruns. Inspect action inputs for checkout-specific
paths, undeclared ambient tools and unnecessarily broad source sets. Stable
independent actions should permit parallel scheduling and cache reuse. Record
remaining coarse actions explicitly rather than claiming full cache granularity
from a successful build.

## Acceptance

- G0: exact reviewed engine/harness/assets/workspace pins, migration/refusal,
  changed-consumer compilation, existing Codex fallback checks.
- G1: complete host browser-to-real-Haskell path, exact replay-free reconnect,
  durable input inclusion, pinned reload, cancellation and retirement.
- Compiler/native: resident certified hit/miss, recursive/boot closure,
  source-hidden fresh consumers, package/family isolation, unused-code omission,
  exact CAF reuse versus distinct private instances, late rollback and final-owner
  reclamation. A compiler smoke test is not certified resident demand acceptance.
- G2: A parks/B publishes/A resumes without erasing B; completion-order shadowing,
  old captures, invalid joins, failure nonpublication, stale proof restaging.
- G3: two children use a capture before parent completion and survive later
  parent failure; separate checkouts, one original pending-claim settlement,
  exact release/admission behavior.
- G4: two parked executions plus progressing third/control; both cancel/commit
  orders, stale completion, pre/post-rename faults, exact recovery/lost-head
  tombstones, retained unconfirmed cleanup.
- Final offline: every just verify constituent, required producer regeneration,
  harness workspace/lint/web gates, matched package and Codex regressions at the
  joined revision. Retain source hashes, commands, executable counts, exit status
  and logs. Keep mock, resident, browser protocol and live evidence distinct.
- G5 remains a separately authorized live trial: browser Sol root, recursive Luna
  component work, Haskell result routing, exact review, repair, integration,
  resource disposition and root interview.

## Parallel execution and delivery

Use up to eight workers around meaningful owners. Sol owns compiler, declaration
validation, native and runtime joins; Luna owns bounded harness/M1, recovery,
packaging and reviews. Root owns contracts and integration. M1 proceeds while
engine work continues. Use isolated worktrees for simultaneous edits. Main/master
may receive verified integration; no branch-preservation requirement remains.
Push verified dependencies then Tidepool once the user configures Git auth.

Concurrent builds use the accepted completion slice; there is no blanket single
compiler slot. Expensive Nix realizations remain serialized at cores=2/max-jobs=1
because the daemon has its own memory cap. Do not restart shared daemons. No live
launch, running-session migration or default-backend switch follows from tests.

## Native Buck checkpoint — 2026-09-29

The atomic-write/repr slice now executes through native `buck2 test`: 8 atomic
unit tests, 2 atomic directory tests, 144 repr unit tests and 56 repr integration
tests passed (210 total, four targets). The source baseline was `56f052612`
plus the accompanying scoped Buck metadata changes. Evidence is retained in
`target/completion-evidence/buck/restored-gate-r3.log`; the command was:

```sh
bash scripts/buck2-run.sh test --print-passing-details --local-only -c remote.enabled=false \
  //bridge/atomic-write:tidepool_atomic_write_unit_tests \
  //bridge/atomic-write:strict_directory \
  //tidepool/repr:tidepool_repr_unit_tests //tidepool/repr:repr
```

The initial run exposed two existing schema rejection regressions, independently
reproduced under Cargo. Commit `56f052612` checks unsupported versions before
current-layout field counts. Both focused Cargo tests subsequently passed
(`repr-cargo-fixed.log`). Eight generator tests pass, covering fixture mapping,
missing inputs, determinism, check-mode nonmutation and failed generation.
The dependency generator stages a scoped manifest/lock/BUCK set, includes dev
inputs, and verifies package identities against the authoritative root lock.
Codex is excluded.

An unchanged four-target build passed with no compile commands
(`restored-warm.log`). This is warm graph reuse, not proof of remote or local
action-cache hits. Earlier controlled C fixture invalidation evidence remains in
`cache-fixture-change-events.jsonl`; repeat that measurement as the graph expands.
Builds and the persistent Buck daemon run in `tidepool-completion-build.slice`
with a 104 GiB aggregate maximum. Root Git and user-systemd probes passed after
the app-server permission repair; worker patch preparation did not require
restarting daemons. Remaining engine, runtime, harness and Haskell migration
gates above remain open.

The subsequent native heap cohort passed 77 unit, 3 GC integration, and 20 raw
scanner integration tests (100 total), retained in
`target/completion-evidence/buck/heap-gate-r2.log`. The first run exposed a test
race masked by Nextest process isolation: concurrent libtest cases changed one
process-global scanner override. Its test guard now holds a mutex and clears
the override before releasing it, including expected panic unwinding. Production
scanner semantics and parallelism of unrelated tests are unchanged. Heap adds no
new third-party dependency closure or external runtime fixture. These checks do
not establish Haskell, codegen, harness, or remote-cache migration acceptance.

Integration test source groups now default to each Cargo target root; the repr
suite explicitly declares its six Rust files. All seven native test targets
passed together (310 tests) after this tightening, with nine generator tests.
A controlled edit to `raw_scan_validation.rs` caused exactly one compile command
while building both heap integration targets; restoring the file also caused
one. Logs: `source-closures-gate.log`, `heap-single-source-change.log`,
`heap-single-source-restored.log`, and `heap-single-source-actions.log` in the
same Buck evidence directory. The unrelated `gc_unit` action stayed reusable.

Do not share a Cargo target directory across differing completion checkouts.
A runtime build after the recovery build reused the latter checkout's old
atomic-write API; rustc identified its source path in
`target/completion-evidence/permission-recovery/runtime-shared-target-failure.log`.
The runtime repair now builds in its own target directory. This is a Cargo
artifact-selection failure, separate from the repaired root permission profile.

## Permission-recovery runtime checkpoints

- `37cad0a0e5` records per-actor durable surfaces, cancellation arbitration,
  and live-scope admission at `PersistentSession`. The repaired isolated Cargo
  target passed 21 recovery/publication tests plus the exact live-scope test.
  Joined engine `3824ad3f38` includes it; `cargo check -p tidepool --lib` passed
  against that join in 1m36s using its own target directory. The logs are
  `permission-recovery/tidepool-runtime-repair-gate7.log` and
  `permission-recovery/tidepool-runtime-owner-gate.log` under this checkout's
  completion evidence; the joined checkout retains
  `joined-public-surfaces-check.log`. Actor publication transport and whole-graph
  revalidation before rename are still pending. A per-actor epoch is not a
  whole-manifest compare-and-swap.
- `e816fffe1b` on `completion/recovery-certification-repaired` preserves exact
  `.hi.owners` bytes and refuses immutable artifact collisions. Its focused
  recovery/certification and exclusive-write gate passed 14 tests, retained in
  `permission-recovery/tidepool-recovery-repair-gate2.log`. It remains a separate
  candidate: integrate with the compiler's package/owner sidecar producer
  contract, not its older branch ancestry. The original compiler/recovery WIP
  remains preserved.

These are component checkpoints. Neither establishes concurrent private
execution, atomic production publication, independent capture delivery, or G3
worker-tree acceptance.

The M2 facade seam is checkpointed separately at `4e3fc0532` on
`completion/m2-facade`: the real browser/Haskell/compaction host test passed
1/1 (119.8s), and request-surface/reload/dispatch policy tests passed 3/3.
Logs are in that checkout's `target/completion-evidence/` as
`m2-compaction-observed.log` and `m2-policy-capture.log`. The fixture observes
settlement of the exact operation before triggering compaction with another
advertised Haskell call; it does not inject `wait_agent`. Independent review
found no blocker. Capture is invoked directly by this test; Haskell checkpoint
token publication and child admission still require acceptance.

Next integration dependencies are explicit:

1. Forward-apply the compiler's package sidecar contract, inherited witnesses,
   and `e816fffe1b` against the joined source. Produce `.hi.owners` after
   `certify_products`, preserving all original groups, zero-group modules and
   the package witness map. Carry owned artifacts across the temporary worker
   directory lifetime into run-owned materialization. Update recovery failure
   classification and exact fixture sidecars together. Keep conflicting witness
   publication fail-closed; owner identity does not yet prove witness stability.
2. Give the runtime one manifest commit owner: stage outside checkout, recheck
   the entire base checksum/high-water under checkout, preflight live promotion,
   hold checkout through rename, then perform infallible visibility installation.
   Restage from current winners plus exact private writes after interference;
   never replay effects. Declaration-tip preflight still needs its owner.
3. Continue Buck with native codegen library/C actions and explicit Cargo cfg
   resolution; migrate its tests only after their full input closure is modeled.

The M2 compiler gate also exposed early RSS rotation after only one or two
requests at 7,283–9,199 MiB against a 7,168 MiB ceiling. Retained evidence is
`permission-recovery/m2-compiler-rss-rotation.log`. A future fresh per-run gate
can use the existing `TIDEPOOL_DAEMON_ARGS` knob to retain more memo state within
the admitted aggregate budget. No global default or active daemon was changed.

## Native codegen and runtime checkpoints (2026-09-29)

Native Buck checkpoint `dcda1c3fe` includes the bignum, bridge, effect and
codegen libraries. Codegen's MD5 build script is represented by a native `cxx_library`
action with declared C/header inputs. The dependency bundle includes normal
library dependencies for these four packages; their broader test dependencies
remain outside this slice. Codex is not part of the migration.

All four library targets built. The native MD5 smoke exercised the public
`session_var_id` API against a fixed independently derived value, forcing the
Rust library and C archive to link. The expanded gate executed 311 tests across
eight native Buck test targets, with no failures. Final generator checks passed 15 tests, including rejection
of new unmodeled codegen build dependencies.
Logs: `target/completion-evidence/buck/codegen-gate.log`,
`md5-smoke-gate2.log`, and `expanded-regression-gate.log` in the same directory.
Final generator evidence is in `final-generators-gate.log` beside those logs.
These are local actions with remote execution disabled; this is not acceptance
of codegen's full test suite or a remote-cache gate.

Runtime binding publication is checkpointed at `df3e1b7df`, joined as
`31e490bf1d`. Five focused tests passed, including promotion of real retained
values, stale-stage rejection and retry, and cancellation before publication.
This remains binding-only: paired declaration publication and actor transport
are pending. Durable authored-generation allocation is the next prerequisite.

Facade checkpoint `cacd0ccf9e` executes the checkpoint effect through a real raw
Haskell call and observes its exact committed operation and successful result.
The focused test passed 1/1; the facade checkout retains
`target/completion-evidence/m2-checkpoint-effect-r2.log`. Checkpoint-backed child
attachment and independent child execution after parent execution failure are
still being implemented and have not passed G3 acceptance.

The compiler candidate passed 24 focused Rust tests, downstream runtime
compilation, the Haskell worker build, and the execution-schema encoder test.
Its full fixture gate compiled 261 Suite targets but stopped at a stale oracle
comparison. Pinned native GHC regeneration confirmed only the fingerprint changed; values
and refusal expectations were identical. The refreshed oracle and full fixture
rerun are pending acceptance; the retained run is
`/tmp/tidepool-compiler-owner/target/prepared-corpus/run.32RorB`.

The compiler candidate was accepted as `4466cdbbc` after the full fixture rerun:
692 Suite execution checks passed, the additional cohorts passed, and seven
embedded prepared artifacts were accepted. The native oracle refresh changed
only its input fingerprint; values/refusals and the payload seal were identical.
Durable authored allocation `95d005f10` passed seven combined allocation and
publication tests, including restart immediately after failed validation.
Joined candidate `d9c3204167` includes both; its facade library check passed.
Logs are retained under `target/completion-evidence/permission-recovery/`; the
joined checkout retains `joined-compiler-runtime-check.log`.

The next native Buck codegen unit target is not accepted yet. Its first run
listed 515 tests: 504 passed, 10 failed, and one was ignored. The test binary
shares process-global heap overrides and an external-descriptor singleton;
ordinary per-case Nextest isolation is absent in the default Buck runner.
Evidence: `target/completion-evidence/buck/codegen-units-gate.log`. An attempted
filter through the built-in executor terminated that executor, retained in
`codegen-isolated-probe.log`; the shared Buck daemon was not restarted.

The next paired-publication prerequisite is a certified authored declaration
carrier. Existing declaration inspection returns source/type information but
no certified module product, so expression products cannot stand in for the
reserved declaration module. Compiler-owned certification must return exact
owned artifact bytes and package/dependency witnesses before runtime can admit
that authored node to the durable graph. Source-less Join certification must
also attest consulted orphan/family package metadata; an empty witness cannot
be assumed merely because the Join has no authored source.

Native codegen unit coverage is now accepted at `b74ca34cc`: 514 tests passed,
one remained intentionally ignored. The Buck-built harness is a build-only
Rust binary with `--test`; a declared Python bootstrap runner executes each
nonignored test in a fresh process, with at most eight cases in parallel.
There is one Rust compile/link action and one aggregate Buck test result; this
does not provide separate cached Buck test results per case. No Cargo build is
nested inside Buck. The runner rejects empty/malformed discovery and requires
an exact one-test success result. Twenty-two generator and runner tests passed.
Evidence: `target/completion-evidence/buck/codegen-units-final-gate.log`.
The prior 10 shared-process failures all pass under process isolation.

Private-only recovery restart admission is checkpointed at `702c90264` with
two focused tests passing. Exact private nodes/artifacts and burned high-water
survive reopening without exposing private declarations; published roots still
require hydration. This does not itself certify or publish new authored nodes.
