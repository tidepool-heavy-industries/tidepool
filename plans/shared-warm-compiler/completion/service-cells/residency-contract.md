# Residency, codegen reuse and item-install contracts

Status: design decision, not implementation or execution evidence.

Scope: the three stale-surface blockers B-03, B-04 and B-05 in
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/completion/stale-surfaces/LEDGER.md`.
The owner is deleting the prepared-install split route
(`PendingPreparedMode`, `PendingPreparedInstall::compile_off_checkout`,
`PendingPreparedSource::Legacy`, `ReadyPreparedSource::Legacy`,
`snapshot_run_prepared`, `revalidate_and_run_prepared`,
`ValueInterfaceSource::LegacyDisk`). Compatibility is not required. This leaf
prevents a guarantee loss while that mechanism disappears.

Base: `e36794e6f9` (`Fold Q2/Q3: one admitted compiler operation, correlated
world items`). The tests cited by the ledger are read at `7f88d89afb`; the two
test files and the `compile_pass`/`compile_fail` fixtures are tree-identical
between that revision and this base. Read
[the campaign plan](../plan.md), [locked contracts](../contracts.md),
[service cells](plan.md), [runtime publication](../runtime-publication/plan.md)
and [tests](../tests/plan.md) before using this document.

Evidence used here is source inspection only. `completion/r1/` contains
`submodule-init.log` and `configure-tests.log`, not a test run.
`q2-admitted-op/test-world-cell-admission-09.log` and
`q2-admitted-op/test-owned-scope-03.log` each record one passing focused case;
neither is B-03, B-04 or B-05 execution evidence. `MERGE-LOG.md` records the
Q2/Q3 trunk argument, not these guarantees.

## Decisions

| id | outcome | owner symbol | test name | parcel that creates it |
|---|---|---|---|---|
| B-03 | **a** — keep the existing residency contract; retire only the split smoke | `PreparedEngine::quiesce_and_collect` (`tidepool/runtime/src/session/prepared.rs:6755`) called by `ResidentSession::complete_prepared` (`tidepool/runtime/src/session/resident.rs:7808`, call at `:7924`) | `prepared_residency::prepared_session_residency_stays_bounded_across_many_turns` (`tidepool/runtime/tests/prepared_residency.rs:605`); delete `certified_resident_turn_compiles_off_checkout_then_installs` (`:836`) | No new parcel. The deletion tranche must retain the residency test. If S1 changes per-cell install cardinality, the tests leaf re-expresses the same bound with the cell's item count. |
| B-04 | **a** — named cell-granularity reuse contract | `PreparedEngine::install` (`tidepool/runtime/src/session/prepared.rs:4251`), `PreparedEngine::codegen_totals` (`:6690`) and `code_export_count` (`:4207`); `ResidentSession::codegen_totals` (`resident.rs:4930`) / `code_export_count` (`:4937`) | `prepared_unit_codegen_cost::a_later_unit_reuses_an_earlier_unit_s_generated_code` (`tidepool/runtime/tests/prepared_unit_codegen_cost.rs:276`), plus a second-cell assertion in `ordinary_demand_history_tests::prepared_actor_entry_late_binding_and_warm_repeat_observe_actual_work` (`exomonad/host/src/actor_host/ordinary_demand_history_tests.rs:507`) | Tests leaf for the joined assertion; service-cells S1 for the world `CellProgram` issuer after `run_turn` is migrated. No new runtime mechanism. |
| B-05 | **a** — runtime state-machine contract for affine consumption and stale/crossed-image refusal | `RuntimeCheckedPrefix::start` (`tidepool/runtime/src/session/admission.rs:1403`) and `CheckedTurnCompletion::settle` (`:1530`); `PersistentSession::admit_checked_item` (`:2126`); `PreparedRuntimeCell::seal` (`tidepool/runtime/src/session/turn.rs:3167`) | `prepared_install_is_consumed_once_by_its_owner`, `prepared_install_refuses_a_stale_image`, `prepared_install_refuses_a_crossed_item`, `joined_source_hook_refusal_precedes_item_settlement` | Runtime-publication R2 (owns `admission.rs`/`turn.rs`) + service-cells S1 (world `CellProgram` issuer) and S3 (world successor settlement); tests leaf creates the tests. |

No blocker has a genuinely dead guarantee. The honest retirements are legacy
*mechanisms*, not risks: B-03's split smoke, B-04's assumption that a unit must
be a separate compiler request, and B-05's `PendingPreparedInstall`
compile-fail controls. Each is replaced below by a production-path assertion.

## Ledger correction that changes B-03 and B-04

The ledger's B-03 row cites `prepared_residency.rs:1-13, 836-877` and says the
test drives `snapshot_run_prepared -> compile_off_checkout ->
revalidate_and_run_prepared` and guarantees bounded residency. The cited range is
`certified_resident_turn_compiles_off_checkout_then_installs`; it only asserts
`ResidentOutcome::Completed`. It never reads `residency()` or `old_bytes()`.

The actual residency guarantee is
`prepared_session_residency_stays_bounded_across_many_turns`
(`prepared_residency.rs:605-704`). It uses
`ResidentSession::run_with_sites` and `run_bind_with_sites`, which survive the
split-route deletion. The only split-route references in that file are in the
separate smoke test (lines 853, 864, 869).

B-04 is also already on the surviving path:
`prepared_unit_codegen_cost.rs` has no `snapshot_run_prepared`,
`compile_off_checkout` or `revalidate_and_run_prepared` reference. It compiles
each unit with `run_turn`, installs with `run_bind_with_sites`, and reads
`ResidentSession::codegen_totals` / `code_export_count`.

Neither test is deleted by deleting the split route. Both must be retained; only
the split smoke and the compile-fail controls retire.

## B-03: major-collection residency stays bounded across turns

### Decision

Outcome **a**. Preserve the residency guarantee on the new service path.
At `de12466597`, the fixture fails at missing source admission before reaching
its measurements. Repair it through the real source transaction, then retain
its independent bound; the existing failure is not execution evidence.

### What the retired split smoke did and did not prove

`certified_resident_turn_compiles_off_checkout_then_installs`
(`prepared_residency.rs:836`) compiles one certified bind with `run_turn`, takes
`snapshot_run_prepared`, calls `compile_off_checkout`, then
`revalidate_and_run_prepared`, and asserts `Completed`. That is a mechanism
smoke test, not a residency assertion. The split API retires, but off-checkout
compiler/native preparation remains required through the existing operation
owner. The replacement also tests independent permitted machine progress while
that work is blocked, plus fresh source/base checks on installation.

### Replacement contract

Owner and entry point:

- `PreparedEngine::quiesce_and_collect` (`prepared.rs:6755`) is the only
  between-turn collection gate. It calls `major_collection_due` (`:6722`), whose
  install-count window is `MAJOR_COLLECTION_INSTALL_INTERVAL` (`:3421`, value 4),
  then `quiesce_and_collect_now` (`:6777`).
- `ResidentSession::complete_prepared` (`resident.rs:7808`) calls it at `:7924`
  for every successful prepared turn whose machine disposition is `Reusable`.
- The surviving entry points are `ResidentSession::run_with_sites`
  (`resident.rs:6989`) and `run_bind_with_sites` (`:8083`). Both reach
  `run_prepared_with_argument` (`:7221`) -> `install_admitted_turn` (`:7356`) ->
  `complete_prepared` (`:7808`). The split route's tail called the same
  `complete_prepared`; the guarantee is not route-specific.

Type and observation:

- `tidepool_codegen::prepared_program::ResidencyCounts`
  (`tidepool/codegen/src/prepared_program/machine.rs:206`) through
  `ResidentSession::residency` (`resident.rs:4922`).
- `usize` old-space bytes through `ResidentSession::old_bytes`
  (`resident.rs:4944`), which is the compacted figure from the last successful
  major collection.

Executable property (keep the existing test body):

- Every turn: `counts.programs <= live_bindings + 1 + N`, where `live_bindings`
  is fixture-known (0 before the bind, 1 after) and `N = 4` mirrors the private
  production constant. This is an independent bound from live bindings, not a
  recomputation of install behavior.
- At every collection boundary (every fourth install), compare the complete
  `ResidencyCounts` value and `old_bytes` with the previous boundary and require
  exact equality. The test requires at least two boundaries in each phase, so a
  no-op collection or a zero-install phase cannot pass vacuously.
- Every turn must return `ResidentOutcome::Completed`; a failed install cannot
  satisfy the count bound.

Failure detected: a program is retained per turn instead of being retired. Then
`programs` grows linearly and the boundary equality fails; `old_bytes` grows
monotonically. The test also detects a changed collection window or a skipped
between-turn collection in the surviving install path.

Smallest fixture: the existing `Notebook::new()` with no effects; 20 turns of
`1 + (2 :: Int)`, then `x <- pure 1`, then 12 turns of `x + (2 :: Int)`. It needs
a resolvable `$TIDEPOOL_EXTRACT`.

Cutover qualification: the test's compile half currently uses `run_turn`. If S1
replaces that with the service issuer, port only the compile half. The
install/collection half and the oracle stay unchanged. If a cell installs more
than one item, the bound becomes `live_bindings + installed_this_cell + N`;
`installed_this_cell` must come from the cell's actual item count, not from
`compiled_off_checkout` telemetry.

### Why not retirement

The split smoke is retired; the residency risk is not. The production collection
mechanism and its between-turn call site survive, but the current fixture has
not reached its assertions. Deleting the test file or the split smoke without
retaining `prepared_session_residency_stays_bounded_across_many_turns` would be
the guarantee loss the ledger warns about.

## B-04: a later cell reuses an earlier cell's generated Cranelift code

### Decision

Outcome **a**, qualified to cell granularity. Cross-cell Cranelift reuse remains
required under one warm compiler service. Do not use service-cells S3 "arm B" as
the contract: that obligation is GHC dependency reuse, not Cranelift codegen
reuse.

### Replacement contract

Owner and entry point:

- `PreparedEngine::install` (`prepared.rs:4251`) resolves declared globals
  through `resolve_imports` against `code_exports` (`:3092`) before compiling or
  installing. `finish_program_install` stages the new exports for later installs;
  `code_export_count` (`:4207`) reports the count.
- `PreparedEngine::codegen_totals` (`prepared.rs:6690`) reports lifetime
  `(functions, code_bytes)`. `ResidentSession::codegen_totals` (`resident.rs:4930`)
  and `code_export_count` (`:4937`) expose the same facts on the service path.
  `PersistentSession::prepared_retained` (`persistent.rs:1536`) is the retained
  import set a later compile receives.
- Install entry point: `ResidentSession::run_bind_with_sites` (`resident.rs:8083`).
  Compile entry point at this base: `run_turn` (ordinary prepared turn). After S1:
  the world `CellProgram` issuer (`admit_world_cell_program`,
  `compiler_world.rs:2526`, wired into product issuance).

Executable property:

- `first.functions > 0` for the first cell's install.
- `second.imports > 0` where `imports = compiled.prepared().globals().len()`. This
  is a shape indicator: the second cell declared imports instead of inlining.
- `second.functions < first.functions`, where each is the delta of
  `ResidentSession::codegen_totals().0` across that cell's install. This is the
  actual reuse evidence: the second install compiled strictly less Cranelift.
- The rendered result remains `[1,2,5,8,12]` for the existing five-unit fixture
  (`a=1`, `b=2`, `c=5`, `d=8`, `e=12`). This is the independent GHC semantic
  oracle, so reuse cannot be achieved by dropping required work.

Failure detected: a later cell regenerates the earlier cell's code because no
code exports were staged or no imports resolved. Then `second.imports == 0`
and/or `second.functions >= first.functions`, while the semantic result can still
pass. A regression in `prepared_retained` (for example, exporting a value binding
that shadows the earlier top, or losing the export on retirement) is also caught.

Smallest fixture: the existing five-unit `Notebook` in
`prepared_unit_codegen_cost.rs` (two trivial binds, one Text unit, one
sum-of-earlier-binds unit, one list unit). The smallest positive control is two
cells: `a <- pure (1 :: Int)` then `b <- pure (a + 2)`; assert cell 2's
`imports > 0` and `functions < cell 1's functions`. Needs `$TIDEPOOL_EXTRACT`.

Real second-cell workflow: extend
`ordinary_demand_history_tests::prepared_actor_entry_late_binding_and_warm_repeat_observe_actual_work`
(`ordinary_demand_history_tests.rs:507`) so the `later` or `warm-repeat`
operation's install reports resolved imports against the first operation's
exports and compiles fewer functions than the entry operation. Cell 1
bootstraps; cell 2 must reuse the warm compiler and the earlier generated code.
Do not replace this with `second_cell_install_compiles_off_checkout`
(`call_timing_tests.rs:157`) or any `any(compiled_off_checkout)` oracle: that
telemetry is being deleted and is not a reuse witness.

### Why not retirement

Whole-cell service compilation does not remove cross-cell reuse. It changes where
the compiler request boundary sits; the runtime still installs exact items and
the later cell still resolves earlier code exports. If S1 eventually returns one
pre-linked whole-cell program with no per-item installs, re-express this contract
at two-cell granularity (cell 1 installs, cell 2 installs with fewer functions
and nonzero resolved imports). Do not silently retire it: the risk is exactly
that all semantic tests stay green while every later cell regenerates its
predecessor's code.

## B-05: affine once-only install and stale/crossed-image refusal

### Decision

Outcome **a**. The old type-level compile tests retire with
`PendingPreparedInstall`; their guarantees move to the runtime item state
machine, which is stronger because it also refuses replay after a success or a
failure, not only a second call the compiler can see.

### Guarantees to preserve

1. **Affine consumption.** One prepared item/install is consumed at most once.
   A started item remains consumed after failure or abandonment and never replays.
   A crossed preflight refusal before admission must not consume the rightful
   owner's still-valid item. Test these transitions independently.
2. **Stale-image refusal.** An item compiled against a superseded public view or
   compile-view digest must not install into the new view.
3. **Crossed-candidate refusal.** An item from one cell/prefix/program must not
   install into a different cell/prefix/program.
4. **No partial publication on refusal.** A refused item changes no binding,
   code export or program count.

### Replacement contract

Owner and entry point:

- `RuntimeCheckedPrefix` (`admission.rs:1279`) owns the private reservation state:
  `in_flight: Option<Arc<ExactCompiledItem>>` and
  `reservation: Option<CheckedItemReservation>`.
- `PersistentSession::admit_checked_item` (`admission.rs:2126`) refuses when the
  item's cell/index/generation does not match the prefix, when `in_flight` or
  `reservation` is already set, or when the public visibility snapshot / compile
  view digest differs from the prefix snapshot. `ResidentSession::admit_checked_item`
  (`resident.rs:4098`) is the public wrapper.
- `RuntimeCheckedPrefix::start` (`admission.rs:1403`) revalidates the exact item
  (`Arc::ptr_eq` against `program.items()[index].native()`), the admission digest,
  the reservation item/generation, the in-flight slot, and both the public
  visibility snapshot and compile-view digest. A mismatch in these checks is
  reported as `SessionError::StaleStagedDeclaration` (`session/mod.rs:481`);
  other failures (for example, an unavailable held-admission store) keep their
  own typed error.
- `CheckedTurnCompletion::settle` (`admission.rs:1530`) revalidates
  `in_flight` pointer identity and, on success, appends the item to the compiler
  prefix, captures the checked native delta, and clears `in_flight` and
  `reservation` exactly once. On refusal it returns before clearing, so the item
  stays burned.
- `PreparedRuntimeCell::seal` (`turn.rs:3167`) and
  `finish_cell_program_admitted` (`turn.rs:3104`) consume the prepared cell and
  seal it into `(CellCheck, Arc<CellProgram>)`; the cell program's items are then
  admitted one at a time.
- Production item route: `begin_cell_program` (`resident.rs:4036`) ->
  `admit_checked_item` (`:4098`) -> `consume_cell_program_item` ->
  `run_with_sites`/`run_bind_with_sites` (which invokes `start` at `:7243` and
  `settle` at `:7932`). The live actor caller is
  `resident_workbench.rs:8556-8582` (prepare/finish/begin) and `:8660`/`:8701`
  (admit/dispatch). At this base that caller still uses finite
  `admit_planned_cell_for_execution`; R1 migrates it to A-06
  `admit_world_planned_cell_for_execution` (`admission.rs:3734`) and A-07
  `admit_world_cell_program` (`compiler_world.rs:2526`).

Executable properties:

- `prepared_install_is_consumed_once_by_its_owner`: reserve one item and run it
  once to `Completed`; replay the same `TurnCode`/execution or the same
  reservation. The second attempt returns `StaleStagedDeclaration`. Assert
  `code_export_count`, `scope_binding_count(scope)`,
  `residency().programs` and `binding_names_in(scope)` are unchanged by the
  refusal. Add a failure control: after a started item fails or is abandoned,
  a restart still returns `StaleStagedDeclaration`; the item is burned, not
  reused.
- `prepared_install_refuses_a_stale_image`: reserve an item, then run a real
  binding turn in the same scope to advance public visibility and the compile
  view digest, then start the reserved item. `start` returns
  `StaleStagedDeclaration`; no program is installed and no binding is added.
  This replaces the split route's `Ok(None)` + in-checkout fallback with a typed
  refusal; per runtime-publication, there is no cold or other retry.
- `prepared_install_refuses_a_crossed_item`: reserve item A from prefix/program
  A, then call `admit_checked_item(prefix_b, item_a)` or
  `prefix_b.start(session, scope, execution_a)`. The attempt returns
  `StaleStagedDeclaration`; state is unchanged. This replaces
  `prepared_install_crossed_image`.
- `joined_source_hook_refusal_precedes_item_settlement`: port
  `joined_source_hook_refusal_precedes_binding_and_native_exports`
  (`resident_workbench.rs:23088`). A source-hook refusal inside the real source
  transaction must precede binding and native exports; assert
  `code_export_count` and `scope_binding_count` are unchanged. Delete the split
  fixture `lazy_binding_install` (`:22535`) and its
  `snapshot_run_prepared`/`compile_off_checkout` body (`:22567`/`:22584`).

Oracle: exact typed error plus pointer/digest identity
(`Arc::ptr_eq` on `ExactCompiledItem`,
`execution.item().admission_digest() == prefix.admission().digest()`,
reservation generation/index equality, and equality of the reserved public
visibility snapshot and compile-view digest) plus observable state before and
after the refusal. The observable state is independent of the success path, so a
test cannot pass by re-implementing the admission check.

Failure detected: duplicate installation (two programs, exports or bindings for
one item), stale installation (an old-view item installed into a new view), or
crossed installation (an item's native definitions installed under another
cell's prefix). Each is the exact risk the old compile-fail controls covered.

Smallest fixture: one tiny checked item, `a <- pure (1 :: Int)`, from a
`CellProgram` with no effects. The stale control adds one
`b <- pure (2 :: Int)` turn. The crossed control needs two prefixes from two
cells. At this base the executable `CellProgram` fixture can come from the finite
`compile_cell_program_admitted` (`turn.rs:2892`) mechanism as a temporary
control, but that is not acceptance: S1's world issuer must supply the final
fixture, and S3/R2 must remove the world refusal at `admission.rs:1599-1603`,
`:2158` and `:2246` before the final-route test can settle a world item.

### Parcel and honesty note

The replacement test is not fully executable on the final route at
`e36794e6f9`: the world `CellProgram` issuer is dormant (A-07), the actor still
uses finite admission, and `CheckedTurnCompletion::settle` refuses world cells
with `WorldCellSuccessorCapabilityMissing` before advancing the prefix or
clearing the reservation. The parcel sequence is:

- A-06/A-07 wire `admit_world_planned_cell_for_execution` and
  `admit_world_cell_program`;
- S1 emits executable world `CellProgram`/checked items;
- R2 adapts `begin_cell_program`, item admission and settlement;
- S3 supplies the item-bound world successor and removes the refusal.

The runtime admission state machine already exists at this base, so this is a
test/fixture and successor-settlement dependency, not a new abstraction.

## Explicitly not proposed

- No `WorldIdentity`-style abstraction, no batching layer, no new certifier,
  registry, query framework, cache or teardown supervisor.
- No compatibility shim, release flag, legacy fallback or second engine.
- No `compiled_off_checkout` proxy for residency or reuse.
- No weakening of exact equality, lifetime, permission or cost assertions.
- No claim that `completion/r1/` contains execution evidence.

## Reading required before implementation

1. Campaign decisions: `plans/shared-warm-compiler/completion/plan.md` G1/G2/G3;
   `contracts.md` (one supported path, affine Fresh/Bound/Consumed, item-specific
   deltas, no new authority owners); `quality/plan.md` Q1-Q10.
2. Leaf plans: `service-cells/plan.md` S1-S3 (especially S1 executable output and
   S3 exact dependency reuse); `runtime-publication/plan.md` R1-R4 (front door,
   item settlement, atomic publication, finite-route deletion); `tests/plan.md`
   joined execution items 1-4 and the new warm-install table.
3. Runtime contributor rules: `tidepool/runtime/CLAUDE.md` (PreparedRuntimeCell
   prepare/close/seal, END authority, affine reservations remain burned, atomic
   publication) and `tidepool/runtime/AGENTS.md` (focused counted checks).
4. Actor contributor rules: `exomonad/actor/CLAUDE.md` and
   `exomonad/actor/AGENTS.md` (duplicate transitions refuse without replacing
   the original result or blocking later valid use; notebook cohort).
5. Residency source: `tidepool/runtime/src/session/prepared.rs`
   (`MAJOR_COLLECTION_INSTALL_INTERVAL`, `major_collection_due`, `install`,
   `quiesce_and_collect`, `residency`, `codegen_totals`, `code_export_count`,
   `old_bytes`); `tidepool/codegen/src/prepared_program/machine.rs`
   (`ResidencyCounts`).
6. Install source: `tidepool/runtime/src/session/resident.rs`
   (`run_with_sites`, `run_bind_with_sites`, `run_prepared_with_argument`,
   `install_admitted_turn`, `complete_prepared`, `begin_cell_program`,
   `admit_checked_item`, and the split symbols being deleted);
   `tidepool/runtime/src/session/persistent.rs` (`install_prepared`,
   `revalidate_and_install_prepared`, `prepared_retained`, `code_export_count`).
7. Item settlement source: `tidepool/runtime/src/session/admission.rs`
   (`RuntimeCheckedPrefix`, `RuntimeCheckedItemAdmission`, `start`,
   `CheckedTurnCompletion`, `settle`, `settle_checked_snapshot`,
   `admit_checked_item`, `adopt_checked_declaration`, the three
   `WorldCellSuccessorCapabilityMissing` refusals); `tidepool/runtime/src/session/turn.rs`
   (`PreparedRuntimeCell`, `prepare_cell_program_admitted`,
   `finish_cell_program_admitted`, `seal`, `TurnPurpose`).
8. Cell program source: `tidepool/toolchain/src/checked_cell.rs` (`CellProgram`,
   `CellProgram::items`, `CellProgramItem::checked_item`, `ExactCompiledItem`);
   `tidepool/toolchain/src/compiler_world.rs` (`WorldSourceSelection`,
   `CheckedWorldCellProgram`, `admit_world_cell_program`);
   `tidepool/toolchain/src/artifacts.rs` (`PreparedCellProgram`).
9. Actor route: `exomonad/actor/src/resident_workbench.rs:8556-8582`,
   `:8660`, `:8701`, `:9159`, `:22535`, `:23088`; read the finite-admission
   call it uses today before replacing it.
10. Deleted tests and controls at `7f88d89afb`:
    `tidepool/runtime/tests/prepared_residency.rs`,
    `tidepool/runtime/tests/prepared_unit_codegen_cost.rs`,
    `tidepool/runtime/tests/compile_pass/prepared_install_*.rs`,
    `tidepool/runtime/tests/compile_fail/prepared_install_*.{rs,stderr}`.
11. Evidence: `stale-surfaces/LEDGER.md`; `completion/MERGE-LOG.md`;
    `completion/r1/` (configure/submodule only);
    `q2-admitted-op/test-world-cell-admission-09.log` and
    `test-owned-scope-03.log` (one passing focused case each, not these
    guarantees).
12. Build and evidence rules: `docs/swarm-builds.md`, `AGENTS.md`
    (`swarm-build`, `buck-out` bind mount, resource admission, retained logs).

## Verification commands for the implementing leaves

These are not run for this design task. Use the admitted environment and retain
logs.

```sh
# B-03, existing integration target
swarm-build just test-target tidepool-runtime session \
  --exact prepared_residency::prepared_session_residency_stays_bounded_across_many_turns \
  --expected-count 1 --jobs 1 --timeout 1800

# B-04, existing integration target
swarm-build just test-target tidepool-runtime session \
  --exact prepared_unit_codegen_cost::a_later_unit_reuses_an_earlier_unit_s_generated_code \
  --expected-count 1 --jobs 1 --timeout 1800

# B-05, runtime unit tests in the owning crate (names to be added with R2)
swarm-build just test-lib tidepool-runtime \
  --exact session::admission::prepared_install_is_consumed_once_by_its_owner \
  --expected-count 1 --jobs 1 --timeout 900

# B-05 actor port, owning actor runner
swarm-build just test-lib exomonad-actor \
  --exact joined_source_hook_refusal_precedes_item_settlement \
  --expected-count 1 --jobs 1 --timeout 900
```

The joined host assertion uses the existing tests-plan command:
`just test-lib exomonad-host --ignored --exact
actor_host::ordinary_demand_history_tests::prepared_actor_entry_late_binding_and_warm_repeat_observe_actual_work
--expected-count 1 ... --compiler-mode owned-resident --trace-profile full`.
Discover exact labels in the admitted environment; do not treat linking,
listing or a zero-match selection as a pass.
