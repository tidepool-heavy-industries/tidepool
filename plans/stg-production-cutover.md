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
7. Verify production parity and demonstrate STG-driven allocation, call,
   force/update, compilation, code-size, and lifetime improvements.

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

## Current evidence

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

These results describe the uncommitted cutover worktree, not `71f23ffd` itself
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

The current neutral prepared fixture was emitted by the documented Cabal probe,
not edited by hand: `haskell/test-prepared-stg/fixtures/m3-vertical.cbor`, SHA-256
`90e8d8c660a3470de5451e0ba2e38050d23b885438a98f8e51ee59bc697eb1b0`.
The two historical float reds were not rerun or compared with main. The three
red focused cases above remain open; full workspace compilation remains unclaimed.

## WIP checkpoint review handoff

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
