# `ResultContract::CallerResult` first slice: commit plan (read-only investigation, not built)

## Design corrections from source
- Joins get the owner's result type at codegen (`emit.rs:342-349` inherited
  destination), but `projectJoin` computes its own contract from the join's
  `resultType` (`ExecutionProjection.hs:768`) and validation checks join
  bodies against the join signature (`validation.rs:977`, `:800`): a join
  inside a representation-polymorphic owner must also project `CallerResult`
  (the shared helper covers it).
- Demand does flow through alternatives, lets and join bodies (`:688`, `:691`,
  `:693`); case scrutinees always get a concrete contract (`:682-684`);
  `signatureForApplication` (`:1267-1275`) needs a `CallerResult` arm.
- Dispatchers match exactly (`apply.rs:36` `find`, `:141` `Exact`).
- Missed site: every top slot gets an adapter from `abis[&id]`
  (`prepared_program.rs:805-815`); a `CallerResult` top needs an adapter per
  instance or none.

## Commits (each leaves the tree green)
1. **Schema, codec, validation, version 8 -> 9** (`execution_schema.rs:83,9`,
   `ExecutionSchema.hs:56,24`, `codec.rs:340-354`, `ExecutionEncode.hs:81`;
   `is_caller_result()`). Validation allows it in function and join
   signatures, `Call` demands, `GlobalDecl::entry_signature`; rejects thunks
   (`:600`), operations, `Enter` (`:765`), case scrutinees (`case_children`
   `:997`), the program entry (near `:1371`); `check_callable` (`:446`)
   requires a `CallerResult` callee for a `CallerResult` demand, never
   oversaturated; `satisfies`/`merge_alternative` exact. Tests: codec
   round-trip (`tidepool-repr/tests/execution_schema_codec.rs`), accept/reject
   table (`execution_schema_contract.rs`), encoder test
   (`haskell/test-execution-schema-encode/Main.hs`). Regenerate every committed
   CBOR in the same commit (model: `37d8a8c51`): `cabal test
   execution-schema-encode --test-options='--write-schema6-fixture …'`,
   `execution-schema-projection <out>` for
   `haskell/test-prepared-stg/fixtures/m3-vertical.cbor`, `cargo test -p
   tidepool-extract-cmd --test import_fixtures -- --ignored` for the import
   fixtures; find how `freer-resume.cbor`/`freer-retention.cbor` are built
   (their `.md` files) BEFORE starting; then `just fixtures-update` +
   `just fixtures-check`.
2. **`EntryAbi` guard**: `lower_internal`/`lower` (`entry_abi.rs:69-92`) return
   `AbiError::UninstantiatedResult`; audit `returned_reps()`/`unwrap_or(&[])` at
   `validation.rs:1220,1227` (accept), `plan.rs:231`,
   `emit.rs:229,241,286,473,499,638,826,1308`, `invocation.rs:279,352`,
   `machine.rs:1444,1627`, `admission.rs:109`, `primitives.rs:18`,
   `floating.rs:17`; `Callee::admits` (`plan.rs:97`) rejects when it cannot
   lower. Unit test in `entry_abi.rs`.
3. **Codegen instances**: entries keyed `(ValueId, Option<Vec<RuntimeRep>>)`,
   one descriptor per function (`plan.rs:323`); `classify` (`apply.rs:115`)
   matches any `Returns` demand for an exact `CallerResult` entry, partial stays
   `Returns[LiftedRef]`, excess refused; `declare_dispatchers` (`:197-301`)
   collects demanded results plus `[LiftedRef]` including closed suffixes
   (`:269-296`); `Dispatchers::exports` (`:46`) one per instance; emission one
   `prepared_entry_{id}_{k}` per instance (`prepared_program.rs:700-715`,
   `emit.rs:137-250`); admission refuses a `CallerResult` program entry and top
   adapter. Tests (hand-built `WireProgram`s): instances at `Int(64)` and
   `LiftedRef` through a dispatcher, cross-program miss as `UnresolvedCallee`,
   admission refusal.
4. **Projection**: `resultContractFor` (`:1303`) returns `CallerResult` when
   `typePrimRep_maybe` is `Nothing` and arity > 0 (thunks, zero arity still
   fail); `projectRhs` (`:607`), `projectJoin` (`:768`), `importedEntry`
   (`:1253`, `LFReEntrant` only). Fixture `haskell/test-prepared-stg/RepPoly.hs`:
   NOINLINE `applyTo :: forall r a (b :: TYPE r). (a -> b) -> a -> b`, NOINLINE
   `atInt n = applyTo I# n`, `atPrim n = applyTo (+# 1#) n`; assert calls to
   `applyTo` with demands `Returns [LiftedRefRep]` and `Returns [IntRep 64]`
   survive (guard against worker/wrapper and specialisation), a join inside
   `applyTo`, and a cross-module import variant.
5. **End to end**: run `RepPoly` in `tidepool-runtime/tests/prepared_execution.rs`
   (register in its suite); re-run
   `shoal_exports_persistent_agents_and_hides_turn_lifecycle_operations`.

## Risks
GHC specialising `applyTo` anyway (probe assertion guards); adapter-per-top
and `EntryMetadata` consumers (`descriptor_bridge.rs`, `link.rs:43-59`)
assuming a concrete contract; dispatcher keys differing only in result
colliding if a `CallerResult` key leaks into `Dispatchers`;
representation-polymorphic `StgOpApp` results still fail; cross-program
lifted-only instances make unboxed foreign demands a typed runtime miss; verify
instance computation for PAPs saturated through `Excess` suffixes
(`apply.rs:283-289`).
