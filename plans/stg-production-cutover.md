# STG production cutover

Implementation starts at `71f23ffd`, the engine merge atop `4db208c6`.
This is ongoing work, not a completed cutover. The detailed engine contracts
are in `haskell-engine-stg.md`; the decisions below supersede its interpreter,
platform, and obsolete-frontend requirements.

## Accepted destination

- One direct prepared-STG JIT, with at least the old production engine's
  supported Haskell/library, effect, and resident behavior. Narrow emitter
  slices are implementation checkpoints, not acceptance.
- Linux x86_64 first. Other production targets reject explicitly; no legacy
  Core backend retained for AArch64. Keep target ABI/layout facts explicit.
- Delete both Rust interpreters. Native compiled GHC is the pure-language
  oracle; explicit contract and Shoal tests cover effects and lifetime.
- Retire the one-shot and REPL MCP endpoints, retaining shared effect/schema
  and output-capture infrastructure used by Shoal.
- Move materialized host values and marshalling to the existing bridge owner;
  do not retain interpreter environments/closure bodies in boundary values.
- Test pruning is authorized. Preserve useful scenarios, not historic test
  counts. No planted-defect or replacement-coverage certification gate.

## Dependency sequence

1. Repair GHC preparation and structured introspection integration. Preserve
   main's compile purpose, memo validity, lookup, and notebook behavior.
   Record/classify initial failures; semantic disposition follows the checked
   engine contract rather than guessed baseline expectations.
2. Complete schema/projection/linking. Carry GHC case classification; verify
   kind, family, representation and component binding. Unmatched refined
   cases produce typed integrity failure, never fabricated values/traps.
3. Integrate one typed entry ABI, descriptor layout, allocation/collection,
   and failure protocol. Prove moving collection during generated execution
   with mixed live references/scalars before broadening expression support.
4. Complete direct lazy execution: calls, joins, multiresults, thunks, PAPs,
   primitives, typed effects, and deterministic cancellation settlement.
5. Integrate initial/incremental/resumed execution through existing machine,
   session, continuation, and code/global lifetime owners. The layered
   `resident_workbench.rs` additions are explicitly owned by this integration;
   inspect their introspection boundary early and preserve main's notebook
   semantics throughout.
6. Remove obsolete engines, interpreter-dependent paths, endpoints, and
   scaffolding. Version artifacts/ABI/worker protocols together. Drain or
   terminate old machines; never migrate live closures or replay effects.
7. Verify production parity and remove all obsolete engine paths. Verify zero
   allocation host calls on generated fast paths through emitted IR or contract
   counters. Comparative performance measurements are not an acceptance gate;
   no main performance baseline was captured.

## Accepted checkpoint-review changes

These decisions supersede the transactional prepared nursery at `e1a4b9145`.

- Fix current heap-reader admission first: observation/forcing, raw root export,
  handle minting and ownership transfer reject an unavailable machine with its
  retained first cause. Preserve missing-handle semantics inside Result.
  Metadata-only release/counting remains available. Gate previously exported
  root consumers before dereferencing; eliminate cross-boundary raw slots in
  the resident STG integration.
- Integrity failure retires the whole machine, including sibling realms sharing
  its heap. It is not cancellation or a recoverable language failure. Retain
  both semispaces and code/descriptor owners through native unwinding; cleanup
  follows allocation/registry metadata, never damaged objects.
- Replace per-object publication/registration and relocation staging with
  pinned descriptor lookup, an exact object-start bitmap built by a linear
  allocation walk, and forwarding Cheney copying. No reachable-graph preflight
  is required under terminal retirement. Validate before dereferencing; retire
  on a late corrupt edge. Destination capacity is at least source capacity.
  After a successful copy, publish a consistent heap before any fallible growth
  allocation; use exact copied live bytes to enforce the existing heap ceiling.
- Use the supported word-aligned scalar profile: 8-byte object alignment and
  at least 16 bytes per object for forwarding. Reject unsupported scalar widths
  rather than maintaining 128-bit-only test machinery. Pinned-GHC vector forms
  remain explicitly inventoried, not silently treated as 64-bit scalars.
- Specify live, forwarded, evaluating/blackhole, and updated-indirection header
  states and word-8 interpretation before rewriting the collector. Persistent
  language-error storage and update restoration must obey that same layout;
  do not overwrite live captures before the update obligation owns them.
- Reserve each recursive allocation group in one checked operation, then
  initialize every sibling without a safepoint before publishing the group.
- Thread demanded result representations through projection, allocate fresh
  IDs for void parameters, and preserve GHC emission order while replacing
  quadratic lookup/reachability work. Carry authoritative constructor tags and
  family size; do not reconstruct complete families from encountered members.
- Replace hardcoded fixture IDs with symbol selection during regeneration.
  Use explicit callable-import evidence for the import-contract fixture.
- Delete disconnected NativeValue/result-area/join/thunk wrappers as the real
  owners land. Preserve their actual semantics in production contracts, and
  restore the value-only effect-routing property test.
- Complete the pinned-dialect pure GHC oracle before resident cutover. Preserve
  notebook, lookup, effects, continuation and resource custody in the existing
  session owners; MachineState is the sole disposition owner.
- In-scope friction work: remove pure-test dependency fanout, improve fixture
  selection/freshness diagnostics, and use existing command/build records with
  exact selections. Recipe selection is conditional on a concrete Shoal
  cutover test needing it; otherwise defer it. Add no legacy support hierarchy,
  fixture-manifest framework, cache, or process supervisor.

## Verification ownership

- One pure-source harness compiles fixture executables under pinned GHC and
  the workbench's `EVAL_PRAGMAS` dialect, including monomorphism/defaulting
  behavior. Any harness-only differences must be explicit. Observe functions
  by application and lazy values with finite demand contexts.
- Engine contract tests own malformed input, ABI, heap/root correctness,
  typed failure, and a named deterministic cancellation-interleaving test
  spanning generated allocation/call/backedge/update points.
- Shoal integration owns notebook/lookup, effects, suspended siblings,
  retained values, retirement, and committed-prefix/recovery behavior.
- One readable support inventory must be generated from owning definitions
  or maintained as a single file referenced by them. Supported production
  forms cannot be marked unsupported to close a milestone.
- Compile changed targets and use focused checks while implementing; run
  the retained broad gates at integration points. Record commands and actual
  results. An interrupted build or zero selected tests is not verification.

## Agent allocation

Terra agents handle bounded reads, caller migrations, protocol regeneration,
endpoint removal, test consolidation, and focused verification. The lead owns
case/ABI/heap/thunk contracts and production integration. Shared-boundary
decisions precede independent implementation. Serialize expensive builds;
use at most three concurrent subagents. Handoff for the model downgrade only
after the hard integration/semantic work is done, not at a passing toy slice.

## Evidence at checkpoint `e1a4b9145`

- Before implementation: workspace compilation unknown; two reported float
  tests red (`just suite tidepool-codegen`), main baseline not established.
- Preparation/introspection wiring and shared material-value migration are
  implemented. Both Rust interpreters and their differential-test machinery
  have been removed; production still uses the old Core JIT.
- Schema v2 preserves case classification, null address, representation-aware
  rubbish, static byte bindings, and constructor result representation. The
  pinned-GHC projection test passes, including a multi-component argument call.
  `docs/stg-projection-inventory.md` is the support-inventory home. Imported
  value representation, optional known entry ABI and evaluatedness now survive
  projection/validation/linking; 33 focused repr tests passed. Runtime import
  handle resolution and tag facts still need completing.
- ABI v2 introduces the compact descriptor header. Live/forwarded states and
  descriptor-only staged collection are implemented; seven focused heap tests
  passed. The active MachineState GC owner now dispatches prepared collection
  after the same checked root snapshot, and native entry uses VMContext nursery
  allocation and noncollecting descriptor publication. Two owning GC tests pass:
  reserved growth with a live mixed-layout cycle, and capacity failure without
  mutation. A native adapter/Tail test now forces nursery collection with a
  live managed argument and proves its stack-map relocation while raw Address
  bits remain unchanged. Thunk update states and resident production integration
  are not established by this work. Runtime failures retain the existing
  MachineFailure cause/disposition pair across the native boundary.
- Full native lowering, effects/cancellation, resident cutover and Core-JIT
  removal remain pending. Workspace compile, fixture consistency and broad
  production parity are not established by the focused checks above.
- `fixtures-check` now checks source fingerprint and regenerated artifact bytes,
  not execution semantics: its old evaluator invocation was removed with the
  interpreter. The native-GHC semantic oracle remains a separate pending task.

## Working checkpoint checks — 2026-09-12

These results describe the cutover worktree committed as `e1a4b9145`, not `71f23ffd` itself
and not a green workspace. Commands below enter the repository toolchain.

| Actual command | Observed result |
|---|---|
| `bash scripts/dev-shell.sh bash -lc 'cd haskell && cabal test execution-schema-projection'` | 1/1 passed after the global import ABI correction. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-repr --lib execution_schema::` | 33 passed, 0 failed, 181 filtered. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-heap --lib execution_descriptor::tests -- --nocapture` | 7 passed, 0 failed; rerun at checkpoint after fallible metadata-staging changes. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-codegen --lib descriptor_bridge::tests -- --nocapture` | 6 passed, 0 failed. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-codegen --lib host_fns::gc::tests::prepared_ -- --nocapture` | Initial allocation batch: 2 passed, 0 failed, 158 filtered; one unused-assignment warning subsequently removed. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-codegen --lib prepared_ -- --nocapture` | Final allocation batch: 6 passed, 0 failed, 156 filtered; includes generated-live-root relocation and three GC-owner tests. Earlier attempts stopped at compilation: runtime error classifier lacked the new typed failure variant, then the new unit harness moved its signature before adapter declaration. Both compile errors corrected; no assertions weakened. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-runtime --lib session::prepared::tests -- --nocapture` | 2 passed, 0 failed, 156 filtered; runtime classification preserves first cause separately from final machine disposition. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-codegen --test prepared_native` | Compiled; 2 passed, 2 failed. Both `generated_descriptor_object_survives_registered_moving_collection` and `linked_function_runs_through_tail_adapter_pap_and_update_control` returned `Unsupported("non-parameter local constructor field")`. Assertions and IDs were not changed after failure. |
| `bash scripts/dev-shell.sh cargo test -p tidepool-toolchain --lib` | Compiled; 88 passed, 1 failed. `prepared_artifact::tests::mismatched_import_contract_is_rejected` failed its `fixture must declare a callable import` prerequisite. Agent ran the whole lib target rather than the requested prepared-artifact filter. |
| `bash scripts/dev-shell.sh just fixtures-update` followed by `bash scripts/dev-shell.sh just fixtures-check` | Passed after fresh generator builds; canonical corpus byte contents unchanged, fingerprint regenerated. This is freshness evidence, not semantic execution. |

That checkpoint's neutral prepared fixture was emitted by the documented Cabal probe,
not edited by hand: `haskell/test-prepared-stg/fixtures/m3-vertical.cbor`, SHA-256
`90e8d8c660a3470de5451e0ba2e38050d23b885438a98f8e51ee59bc697eb1b0`.
The two historical float reds were not rerun or compared with main. The three
red focused cases above remain open; full workspace compilation remains unclaimed.

## Review handoff at `e1a4b9145`

This checkpoint is on `engine/stg-production-cutover`, based on `71f23ffda`.
It is not a production cutover or a green-workspace claim. The substantial
interpreter/endpoint removals are intentional under the approved plan; the
replacement compiled-GHC oracle is still pending.

Review the connected boundaries first:

1. `ExecutionProjection.hs` → schema v2/codec/validator/linker: preserved case
   classification, entry-vs-site arity, void positions, global representation
   and optional entry evidence, rubbish, null addresses, constructor results.
2. Compact descriptor ownership and staged copying → MachineState checked
   roots → generated nursery allocation/publication. In particular, check
   alignment, reserve/growth failure before mutation, metadata lifetime and
   first-cause/final-disposition propagation. The live-argument GC test exercises
   the real adapter/Tail call chain, not just post-return collection.
3. Shared bridge values and interpreter/endpoint deletions: retained consumers
   must not acquire interpreter closure state or lose Shoal functionality.

Known hard work remains: broad expression/operation lowering, executable import
resolution, tag/capture metadata completion, thunk states and updates, effect
suspension/resumption and deterministic cancellation interleavings, resident
workbench reconciliation/cutover, the pure GHC oracle, and old Core JIT removal.
The present native entry still accepts only a narrow constructor-producing
slice. A connected allocation boundary is progress, not production parity.

The prepared collector currently covers the descriptor nursery; integration
with external payloads and generational/old-space ownership remains unproven.
Descriptor-copy staging allocations are fallible; this is not a claim that
every allocation in the existing shared root-capture machinery is fallible.

## Post-review implementation evidence

The current tranche extends `e1a4b9145`; it does not establish production
cutover. Schema v3 adds GHC's one-based constructor tag and family size while
retaining ABI v2. Validation checks tag bounds, per-family size agreement and
tag uniqueness, without demanding that the wire inventory enumerate a complete
family. Scalar representations above 64 bits are rejected; vector support is
still a separate inventory obligation.

Projection now threads demanded result representations through calls, cases,
closure bodies and joins, and allocates distinct IDs for void parameters.
The focused Haskell regression includes a genuinely oversaturated application
of a polymorphic function, not merely a call whose declared result already
matches its demand.

Current-machine observation, forcing admission, root export, handle minting
and ownership transfer now reject terminal disposition before accessing heap
values. A regression injects cancellation followed by integrity failure and
checks that readers preserve the first cause while reporting Unavailable.
A second regression now provokes an interior-pointer failure after the root
and its self-edge have actually moved. Observation/forcing, slot export and
entry reject the resulting first cause; both source and destination buffers
remain owned through unwinding, and metadata-only handle release succeeds.

The prepared collector now uses pinned descriptor lookup, a reusable exact
object-start bitmap and deduplicated root-slot scratch. Cheney forwarding
replaces the per-object registry and staged relocation plan, which are deleted.
Ordinary collections reuse two semispaces; growth follows exact copied live
bytes. A completed copy is published even if subsequent growth is rejected.
The current prepared heap rejects non-null managed references outside its
initialized region: external payload/old-space integration is still required,
not implicitly supported by leaving those pointers untraced.

Descriptor headers now define Live, Forwarded, Evaluating and Updated states.
Updated thunks trace only their word-8 indirection target; evaluating thunks
retain their original capture layout. This is the layout contract, not evidence
that generated thunk entry/update lowering is complete.

Completed focused checks in this tranche (all through the repository dev shell):

| Selection | Result |
|---|---|
| `cargo test -p tidepool-codegen --lib unavailable_machine_ -- --nocapture` | 2 passed. |
| `cargo test -p tidepool-repr --lib execution_schema:: -- --nocapture` | 36 passed. |
| `cargo test -p tidepool-heap --lib execution_descriptor::tests -- --nocapture` | 9 passed. |
| `cargo test -p tidepool-heap --lib` | Final collector integration: 37 passed, including six exact-start/terminal-copy contracts and retained Core-heap tests. |
| `cargo test -p tidepool-codegen --lib entry_abi:: -- --nocapture` | 5 passed. |
| `cd haskell && cabal test execution-schema-projection` | 1/1 passed, including oversaturation and distinct void parameter IDs. An earlier test-helper type error was corrected before execution. |
| `cd haskell && cabal test execution-schema-encode` | 1/1 passed. |
| `env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER just fixtures-check` | Passed after the documented neutral-fixture probe; canonical corpus unchanged. |
| `cargo test -p tidepool-effect --test proptest_effect` | 3 passed; restored value-only routing coverage uses the bridge owner. |
| `cargo test -p tidepool-testing --test proptest_varid_defense` | 6 passed after moving these unchanged scenarios out of repr's dependency graph. |
| `cargo test -p tidepool-testing --test proptest_cbor` | 8 passed, 1 red: `literal_round_trip`, minimal `LitFloat(4294967296)`, rejected as float bits exceeding u32. Source inspection identifies this as pre-existing; no main baseline run was used, and it is not conflated with the separate historical float tests. |
| `cargo test -p tidepool-toolchain --lib prepared_artifact::` | 3 passed after adding a real known callable import to the GHC fixture. |
| `cargo test -p tidepool-codegen --lib prepared_ -- --nocapture` | 7 passed after collector replacement, including generated live-root relocation, late corruption, semispace reuse and published growth failure. |
| `cargo test -p tidepool-codegen --lib descriptor_bridge::` | Final descriptor collector consumers: 6 passed. |
| `cargo test -p tidepool-codegen --test prepared_native` | Final schema/fixture/collector integration: 4 passed. Symbol lookup replaces numeric selectors; behavioral assertions retained. |
| `cargo test -p tidepool-repr --test execution_schema_contract` | 1 passed. |
| `cargo test -p tidepool-codegen --test resident -p tidepool-runtime --test boundaries --test session -p tidepool-harness --test selfharness --no-run` | Compiled all selected targets after repairing the stale `EffectRoster` caller. Tests were not run. |
| `cargo test -p tidepool-handlers --lib minimal_stack_decls_keep_interposed_effects_after_handlers` | 1 passed. |
| `cargo test -p tidepool-runtime --lib session::prepared::tests` | 2 passed on the final collector/fixture state. |
| `cargo test -p tidepool-actor --lib resident_reentry_state_tracks_unavailable_busy_and_stale_checkout` | Actor lib tests compiled; focused test passed (1). Earlier `--no-run` found a stale deleted-helper reference; the literal-only fixture now uses the existing standard constructor table. |
| `just suite-check` | Passed: every integration file registered exactly once. |

That earlier regenerated neutral fixture SHA-256 was
`d2dc64aec2c7162aceb5620cd5e6bd8eb0e3b9516ed3cfbb4f4f128a5f79beb1`.
The historical float failures were pre-existing by source inspection and were
not rerun or compared with main. Full workspace compilation and production
parity remained unclaimed at that checkpoint.

The first collector integration run had one failure: the old growth test
required the final root's numeric address to differ from its original address.
Two-copy growth can legally reuse that allocation address. The test now checks
published heap ownership and contents, repeated semispace reuse, and a
post-copy capacity rejection. The runtime's matching address-comparison bug
was fixed with an explicit completed-copy signal for cursor publication and
generation invalidation. The final seven-test selection above passed after
that correction and after the emitted-IR assertion for zero fast-path calls.

Zero-argument `StgApp` now follows the pinned GHC lone-variable convention:
zero-bit values return no atoms; a single unlifted component returns its atom;
lifted values retain entry behavior. A real `NOINLINE Int#` identity fixture
asserts direct return of its own parameter, and the projection/freshness checks
pass after regeneration. Multi-component binders remain explicitly rejected
as a violated post-unarisation invariant. `VarEnv` lookup and a `UniqSet`
worklist replace quadratic binder lookup/closure expansion while emitted
bindings retain GHC's original dependency order. Worklist-only byte stability
was not isolated from the intentional fixture additions.

## Next connected lowering boundary

### Accepted stack-safety and traversal revision

Before extending lowering, schema v4 uses one flat postorder expression arena
per program, with explicit top-level body root indices. Nested closure, join
and alternative bodies are indices into that same arena. The envelope now has
13 fields: expressions at index 10, top bindings at 11, entry at 12. Scalar
and expression tags are unchanged. Old schema artifacts reject and regenerate;
ABI v2 remains until the managed-reference tagging change lands.

Decode flat records linearly. A nonrecursive CBOR preflight bounds container
nesting by the flat grammar before `ciborium::Value` allocation; byte/node/work
limits remain, while expression-depth limits disappear. Structural validation
checks earlier-child indices, unique syntactic ownership and reachability.
Every ValueId binder is unique program-wide; dense scope tables with undo logs
and closure visibility levels replace environment-map cloning and parent-chain
lookup. Bound sparse numeric namespaces before allocation. Use `recursion`
0.5.4 for the two ordered scope/type traversals and tree analyses; reverse child
scheduling once to retain first-error precedence. Native block emission remains
an explicit control-flow worklist.

Descriptor observation uses finite demand and node/work budgets, not a second
relocation-sensitive cycle registry. Budget exhaustion is a typed observation
failure. Rooted seeds use stable boxed cells and truncate registration before
cell release. Bridge Value stays boxed with its existing iterative Drop;
streaming Display/Debug and uncapped walks become stack-safe through ValueFrame.

Reference tag 7 deliberately means evaluated-with-descriptor-inspection for all
families and function closures; it is not GHC's small-family/arity convention.
Centralize tag construction so future arity evidence does not alter dynamic
apply at multiple sites. Updated-chain evacuation uses a second bitmap for
cycle detection and forwards every traversed thunk to the evacuated result.
No host call may occur between a recursive group's bump and completion of all
sibling headers and fields. These changes precede the native lowering below.

Before broad lowering, review the result/root and retained-code boundary as a
unit. Adding expression cases to the current one-constructor adapter would
extend a temporary wrapper, not complete the production engine.

Target-only projection needs a dependency-source repair before it can be a
production completeness boundary. In the M3 probe, targeting
`polymorphicIdentityResult` retains only that top binding although its full
projection calls local `polymorphicIdentity`; `pmBindings` free-variable
evidence omits that edge. Exact-symbol lookup fixes ID aliasing, not missing
dependency evidence. Keep the full-program projection as the checked input
until the target closure is proven complete.

Schema-v4 checkpoint verification: the full `tidepool-repr` suite, focused
bridge deep-value tests, prepared-native, prepared-runtime, toolchain artifact,
CBOR property, Haskell projection, canonical fixture freshness, and suite
registration checks passed. Workspace `cargo build --workspace --tests` did
not complete: at that checkpoint, the reported `InlineInput` failure was
attributed by source inspection to the macro code introduced by `e1a4b9145`,
not to unchanged `main`. This is a historical attribution, not evidence that
the current workspace is green.

1. `entry_abi.rs`, `descriptor_bridge.rs` and `MachineState`: choose one typed
   status/multi-result transport and root managed result slots across later
   safepoints. Keep lifted/unlifted distinctions in the owning value contract.
   Caller-area transport is the current generated implementation; register
   transport must not be claimed merely because the ABI model can describe it.
2. `prepared_native.rs`: compile and retain one linked program, declaring all
   local entries before definitions and pinning code/descriptors together.
   Lower Return/direct Call, then descriptor-backed Enter and classified Case
   through that same ABI. Root live arguments and scrutinees with the existing
   stack-map owner. Unmatched alternatives report typed integrity failure.
3. Real heap thunk entry/update owns capture lifetime and cancellation/error
   settlement. Delete the disconnected Rust memo/result/PAP sketches as these
   owners land; do not give them another compatibility wrapper.
4. Linking currently checks import metadata, not executable handles. The
   binding/session owner must supply opaque callable/value handles with code
   and heap custody. Retained imports require shared heap ownership or a
   validated external-space owner before the collector can trace them.
5. Integrate compilation retention and results into `session/prepared.rs` and
   notebook execution together. Current prepared runs still compile a single
   constructor-producing entry and return `NativeConstructor`; production
   `resident_workbench` still dispatches Core. Keep the GHC oracle ahead of
   actual resident cutover, and preserve main's notebook/lookup behavior.

This is the next high-value joint review boundary: result rooting, imported
value custody, and retained compiled entries constrain one another. The
collector checkpoint does not settle those choices by accident.

## Wave 3 fold evidence — 2026-09-12

The fold ran at working-tree revision `204604dc19ce526dde3d717f3c8e0078714a5002`
(dirty, with the listed Wave 3 changes). Fixture regeneration and the owning
prepared checks completed. This does not claim a production cutover: G's
generic/repro cases remain red, the workspace compile-only gate and changed
inner loop are blocked by unrelated formatting/source issues, and full
production parity remains unclaimed.

The implementation handbacks currently report:

- A: 49 schema tests passed after the validator/projection-scope correction.
- B: the corrected projection regression passed (1/1), including the target
  closure case.
- C: the retirement is correct after Terra's lock repair: 2 additions and
  339 deletions, with no retained upgrades; `tidepool` library check passed.
- D: tag tests 4, descriptor tests 10, descriptor-bridge tests 7, prepared
  native tests 1, and ABI-rejection coverage 1 passed. Fixture regeneration
  and the final prepared integration fold are recorded below.
- E: Terra's correction passed 49 heap tests plus 3 prepared-GC tests, and the
  forwarded incoming-tag regression passes after the root-order fixture fix.
  `descriptor_at` now uses the raw pointer/local dereference contract, and the
  capacity-growth test passes.
- G: residual deleted-interpreter helper tests were migrated. The affected
  runtime suites compiled; `jit_deterministic1` passed, and
  `captured_real_core` had 3 passes and 1 ignored test. After fresh producer
  regeneration, `generics generic_deriving_337` ran 11 tests with 0 passed and
  11 failed; the repro-339 selection ran 1 test with 0 passed and 1 failed.
  Failures were `InvalidPreparedRepresentation`, duplicate top-level `sat`, or
  prepared `InvalidSignature("constructor fields [Word(32)] do not match
  [Word(64)]")`, rather than the earlier generic `UnsupportedTarget` report.

Regeneration used the documented probe
`bash scripts/dev-shell.sh bash -lc 'cd haskell && cabal run execution-schema-projection -- test-prepared-stg/fixtures/m3-vertical.cbor'`,
then `bash scripts/dev-shell.sh just fixtures-update` and
`bash scripts/dev-shell.sh just fixtures-check`. The final neutral fixture
SHA-256 is `36bf360c1f38a67dc736d97d9af782c7c495f2f75ef6206ca79d93f911c78b4e`.
The canonical freshness check passed; the initial update required rebuilding
`tidepool-extract-cmd` and `tidepool-extract-bin` because the resolved worker
was stale.

Owning fold checks passed: prepared native 4/4, runtime `session::prepared`
2/2, and toolchain `prepared_artifact::` 3/3. Workspace
`bash scripts/dev-shell.sh cargo build --workspace --tests` reached all crates
but failed in `tidepool-actor/tests/resident_local_actor.rs` because
`sibling_server` is undefined at lines 321 and 329 (with inferred-type errors
and one unused-import warning). `bash scripts/dev-shell.sh just changed
204604dc19ce526dde3d717f3c8e0078714a5002` stopped before tests on rustfmt
diffs across the changed-file set; no changed tests executed. The final
`bash scripts/dev-shell.sh just fixtures-check` passed.

The differential/proptest scenarios removed during retirement are recorded as
dropped coverage, while explicit JIT assertions remain. No new replacement
coverage or planted-defect certification is claimed. Trial-round accounting
is recorded below. Lead assignment/review/correction interventions are counted
where supplied by those handbacks; complete lead totals and worker or planner
token usage are unavailable from the harness. Brief-size insufficiency remains
explicit, while E's sub-item escalation is now resolved.

| Parcel | Worker/tool rounds | Outcome path |
|---|---:|---|
| A | 4 | Two verification continuations and one phase clarification; 49 schema tests passed. |
| B | 3 | Correction plus verification; projection regression 1/1 passed. |
| C | 2 + Terra 1 | Lock repair complete; 2 additions, 339 deletions, no retained upgrades. |
| D | 1 | Lead correction messages; tag/descriptor/bridge/native/ABI selections passed. |
| E | 3 + Terra repair | 49 heap + 3 prepared-GC tests and forwarded-tag regression passed. |
| G | 3 | Residual helper migration complete; generic/repro reruns remain 0/11 and 0/1 after regeneration. |
