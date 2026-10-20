# Cell preparation received boundary (R1 ↔ S1)

Status: frozen received boundary for the shared-warm-compiler completion wave.
This document is a contract, not implementation or execution evidence. It fixes
the values that cross between runtime publication (R1) and service cells (S1) so
the two owners can add types in their own crates without a shared-file stub.

Source of truth:

- Freeze base: `e36794e6f9` (`completion/boundary-freeze`).
- R1 draft: `64a5573930` (committed; `7d4d9efd70` + `64a5573930`),
  `tidepool/runtime/src/session/{admission,turn}.rs` and
  `tidepool/runtime/tests/checked_quoter_program.rs`. R1 is still based on
  `7f88d89afb`, not on the freeze base.
- S1 draft: `7f88d89afb` clean tree; its `service-cells/plan.md` is the draft.
- `7f88..e367` changes only Haskell request/pipeline code and a plan report.
  Toolchain/service-cell Rust line numbers are valid on `e367`, `7f88` and
  `64a5573930`. `admission.rs` and `turn.rs` line numbers are from R1's
  `64a5573930`, which owns those files; `(R1)` marks refs proven against it.

Owner table used here (from `completion/plan.md` and the leaf plans):

| Area | Owner | Owned paths for this boundary |
|---|---|---|
| Service cells (S1) | engine integration | `tidepool/extract-cmd` request/client schema, `tidepool/toolchain/src/{cell_plan,compiler_world,checked_cell,artifacts,declaration_context}.rs`, `bridge/haskell` request/completion/GHC pipeline |
| Runtime publication (R1) | engine integration | `tidepool/runtime/src/session/{admission,turn,persistent,paired_publication}.rs` and actor workbench callers |
| Lifecycle | lifecycle integration | original-source admission, source A/target B retention, `source_admission.rs`/inspection-world base, generation gates |
| Native selection | engine integration | exact native resolution, first publication, current projection |

No new owner is introduced. Where a value is owned by lifecycle or native
selection, R1/S1 consume it; they do not re-own it.

The one-line boundary:

> In: authored `WorldCellRecipe`, the service-issued parser plan and runtime
> reservation, original source A and target placement B, and the runtime's
> admission owner. Out: exactly one executable `CellProgram` with checked
> items, demanded execution evidence, and per-item private successor
> descriptions, sealed only after a clean compiler END.

## 1. Values crossing the boundary

Each subsection gives the exact type, owner, fields/variants with status tags,
the one constructing function, the one consuming function, and what the value
must not carry.

Status tags:

- `exists-today`: present at `e36794e6f9`.
- `added-by-R1`: present only in R1's current draft.
- `added-by-S1`: required by this contract; S1 must add it in its own crate.
- `removed`: must not survive this boundary.

### 1.1 Authored `WorldCellRecipe`

Type: `tidepool_extract_cmd::WorldCellRecipe`
Owner: `tidepool-extract-cmd`, `src/request.rs` (service-cells).

Fields (`request.rs:138`):

- `source: String` — `exists-today`. Authored cell source.
- `template: String` — `exists-today`. The parser template for this cell.
- `turn_templates: Vec<(String, String)>` — `exists-today`. Kind/body pairs for
  the authored turn templates.

Constructor: `WorldCellRecipeDecoder::world_cell_recipe`
(`tidepool/extract-cmd/src/request.rs:1715`) decodes the wire recipe. The
authoritative world-path constructor is `compiler_world::plan_cell`
(`tidepool/toolchain/src/compiler_world.rs:1934`), which sends the recipe to the
service parser; `cell_plan::admit_world_plan`
(`tidepool/toolchain/src/cell_plan.rs:539`) retains the same `Arc<WorldCellRecipe>`
in the issued plan.

Consumer: `CompilerWorldCellPlan::recipe`
(`tidepool/toolchain/src/cell_plan.rs:197`) and
`admit_world_parser_receipt`
(`tidepool/toolchain/src/cell_plan.rs:568`), which binds the parser receipt to
the exact source/template bytes.

Must not carry:

- No compiler authority, visibility snapshot, include paths, injected modules,
  catalog selection, native imports, interfaces, or finite baseline. The type's
  own doc says the runtime admission and held-world owner supply authority.
- No re-parsed recipe. `admit_world_parser_receipt` checks the source/template
  hashes (`cell_plan.rs:576-585`); callers must pass the plan's retained recipe,
  not reconstruct one from a compile view or a `CheckedCellRequest`.
- No caller-supplied "current transaction" standing in for the issuing source.
  The recipe is authored data; the source transaction is a separate authority
  input.

### 1.2 Service plan: syntax, ordered items, and the observation-name witness

Types and owners:

- `tidepool_toolchain::cell_plan::ParsedCellPlanKind`
  (`tidepool/toolchain/src/cell_plan.rs:36`) — service-cells.
- `tidepool_toolchain::cell_plan::ParsedCellBindingForm`
  (`cell_plan.rs:46`) — service-cells.
- `tidepool_toolchain::cell_plan::ParsedCellPlanSpan`
  (`cell_plan.rs:52`) — service-cells.
- `tidepool_toolchain::cell_plan::ParsedCellPlanItem`
  (`cell_plan.rs:76`) — service-cells.
- `tidepool_toolchain::cell_plan::ParsedCellPlan`
  (`cell_plan.rs:114`) — service-cells; the finite parser plan.
- `tidepool_toolchain::cell_plan::CompilerWorldCellPlan`
  (`cell_plan.rs:160`) — service-cells; the world parser plan.
- `tidepool_toolchain::cell_plan::CompilerWorldCellReservation`
  (`cell_plan.rs:221`) — service-cells.
- `tidepool_toolchain::checked_cell::CheckedPlannedCellSlot`
  (`tidepool/toolchain/src/checked_cell.rs:710`) — service-cells.
- `tidepool_toolchain::cell_plan::WorldObservationName` — `added-by-S1`,
  service-cells, `cell_plan.rs`.

`ParsedCellPlanKind` variants:

- `Prologue` — `exists-today`.
- `Declaration` — `exists-today`.
- `Bind` — `exists-today`.
- `Expression` — `exists-today`.

`ParsedCellBindingForm` variants:

- `Action` — `exists-today`.
- `Let` — `exists-today`.
- `Recursive` — `exists-today`.

`ParsedCellPlanItem` fields (`cell_plan.rs:76`):

- `index: usize` — `exists-today`. Ordinal in the ordered plan.
- `kind: ParsedCellPlanKind` — `exists-today`. Parser classification.
- `binding_form: Option<ParsedCellBindingForm>` — `exists-today`. Parser's
  native bind form, not source-text matching.
- `binders: Vec<String>` — `exists-today`. Authored binder names for `Bind`;
  empty for other kinds.
- `source: String` — `exists-today`. Authored item source.
- `span: ParsedCellPlanSpan` — `exists-today`. Parser span.
- `source_ordinals: Vec<usize>` — `exists-today`. Source/template ordinal
  correlation.
- `observation_name: Option<WorldObservationName>` — `added-by-S1`. Present
  exactly for `Expression`; issued by the parser/service owner, never derived
  by runtime.

`ParsedCellPlan` fields (`cell_plan.rs:114`):

- `specification: Arc<CellRecipe>` — `exists-today`.
- `include_paths: Vec<PathBuf>` — `exists-today`.
- `producer_sha256: [u8; 32]` — `exists-today`.
- `items: Vec<ParsedCellPlanItem>` — `exists-today`.
- `receipt: Arc<[u8]>` — `exists-today`.
- `observations: Arc<[u8]>` — `exists-today`.
- `digest: [u8; 32]` — `exists-today`.

`CompilerWorldCellPlan` fields (`cell_plan.rs:160`):

- `world: Arc<CompilerWorld>` — `exists-today`.
- `recipe: Arc<WorldCellRecipe>` — `exists-today`.
- `producer_sha256: [u8; 32]` — `exists-today`.
- `items: Vec<ParsedCellPlanItem>` — `exists-today`.
- `receipt: Arc<[u8]>` — `exists-today`.
- `observations: Arc<[u8]>` — `exists-today`.
- `digest: [u8; 32]` — `exists-today`.
- `allocation: Arc<()>` — `exists-today`. Plan-allocation witness.
- `issued: AdmissionIssueGate` — `exists-today`. One-shot issuance gate.

`CompilerWorldCellReservation` fields (`cell_plan.rs:221`):

- `plan: Arc<CompilerWorldCellPlan>` — `exists-today`.
- `admission_digest: [u8; 32]` — `exists-today`.
- `slots: Arc<[CheckedPlannedCellSlot]>` — `exists-today`.
- `expected_bindings: Arc<[Vec<(String, SessionVarId)>]>` — `exists-today`.

`CheckedPlannedCellSlot` variants (`checked_cell.rs:710`):

- `Prologue { declaration: u64 }` — `exists-today`.
- `Declaration { declaration: u64 }` — `exists-today`.
- `Bind { value: u64 }` — `exists-today`.
- `Expression { capture: u64, observation_name: String }` — `exists-today`; the
  `observation_name` string is currently runtime-derived on the finite path.

`WorldObservationName` — `added-by-S1`. Exact fields:

- `name: String` — `added-by-S1`. The exact visible observation name.
- `world_logical_id: [u8; 32]` — `added-by-S1`. The world the name was issued
  for.
- `names_domain_sha256: [u8; 32]` — `added-by-S1`. Commitment to the service's
  current visible-name domain used to prove non-collision, without enumerating
  historical names at the runtime boundary.

Constructor: the parser endpoint's plan issuer,
`compiler_world::plan_cell` (`compiler_world.rs:1934`), which calls
`cell_plan::admit_world_plan` (`cell_plan.rs:539`) and
`admit_world_parser_receipt` (`cell_plan.rs:560`). S1 adds the observation-name
witness to the same parser receipt/plan version; `WorldObservationName` is never
constructed by R1.

Consumer: R1's `PersistentSession::reserve_world_cell`
(`tidepool/runtime/src/session/admission.rs:3958` (R1)) reads
`plan.items()`, `item.index()`, `item.kind()`, `item.binders()`, and (after S1)
`item.observation_name()`, then issues slots through
`CompilerWorldCellPlan::issue_admission_token` (`cell_plan.rs:388`) and
`reserve` (`cell_plan.rs:408`). S1's checked-output issuer consumes the same plan
in `prepare_world_cell_outputs` (`compiler_world.rs:2539`).

Must not carry:

- No second parser inventory or caller-built slot list. Slots are issued and
  validated against the plan's ordered items
  (`validate_world_cell_reservations`, `cell_plan.rs:466`).
- No caller-derived observation name and no historical-name enumeration.
  `valid_observation_name` (`checked_cell.rs:841`) only checks shape; the
  service witness must prove the name is unique for the exact world.
- No finite `include_paths`, `injected_modules`, `ExactCompilationRequest`,
  catalog selection, or baseline. `ParsedCellPlan` is the finite parser shape;
  the world route uses `CompilerWorldCellPlan` and must not borrow finite
  fields to fill it.
- No checked output authority. The plan is syntax/classification and expected
  identities only; `CompilerWorldCellReservation` carries no
  `ExactCompiledItem`, `CellProgram`, or native permission.
- No reuse of one plan for a second execution. `AdmissionIssueGate`
  (`cell_plan.rs:176`) is one-shot; a refused reservation must fail before
  claiming it.

Divergence note: R1's draft currently refuses every `Expression` before this
value exists (see D4). That refusal is a temporary guard, not the contract.

### 1.3 Runtime-issued reservation and admission

Types and owners:

- `tidepool_runtime::session::RuntimeCellAdmission`
  (`tidepool/runtime/src/session/admission.rs:645`) — runtime-publication.
- `tidepool_runtime::session::RuntimeCellCompilerBasis` (`admission.rs:659`) —
  runtime-publication.
- `tidepool_runtime::session::RuntimeFiniteCellInputs` (`admission.rs:679`) —
  runtime-publication.
- `tidepool_runtime::session::RuntimeCellPurpose` (`admission.rs:707`) —
  runtime-publication.
- `tidepool_toolchain::checked_cell::AdmissionDigest` — service-cells; consumed
  by runtime.

`RuntimeCellAdmission` fields (`admission.rs:645`):

- `owner: Arc<RuntimeAdmissionOwner>` — `exists-today`.
- `owner_epoch: u64` — `exists-today`.
- `session: SessionId` — `exists-today`.
- `purpose: RuntimeCellPurpose` — `exists-today`.
- `_retained_scope: Arc<RuntimeLexicalScopeLease>` — `exists-today`. Target B.
- `prefix_started: AtomicBool` — `exists-today`.
- `scope: ScopeId` — `exists-today`.
- `reserved_generations: Vec<Generation>` — `exists-today`.
- `initial_value_generation: Generation` — `exists-today`.
- `compiler_basis: RuntimeCellCompilerBasis` — `exists-today`.
- `digest: AdmissionDigest` — `exists-today`.

`RuntimeCellCompilerBasis` variants (`admission.rs:659`):

- `Finite(RuntimeFiniteCellInputs)` — `exists-today`; removed from the world
  route by this contract (the variant may survive only for the finite route
  until R1/S1 delete it).
- `World { reservation: Arc<CompilerWorldCellReservation>, private_key: CompileViewKey, original_source: Arc<RuntimeLexicalScopeLease> }`
  — `exists-today`. The world reservation, the private compile-view key, and
  original A. `original_source` is retained here today with a temporary
  `dead_code` allowance; this contract makes it reachable (see 1.4).

`RuntimeCellPurpose` variants (`admission.rs:707`):

- `Observation` (`#[cfg(test)]`) — `exists-today`.
- `PrivateExecution(Arc<PrivateExecutionAdmission>)` — `exists-today`.
- `NativeSetup` — `exists-today`.
- `HostCarrier { binding: String, expected: HostBindingType }` — `exists-today`.

Constructor: R1's `PersistentSession::reserve_world_cell`
(`admission.rs:3958` (R1)) is the only world-reservation kernel. It calls
`CompilerWorldCellPlan::issue_admission_token` (`cell_plan.rs:388`) and
`CompilerWorldCellPlan::reserve` (`cell_plan.rs:408`), then constructs
`RuntimeCellAdmission`. S1 must not construct `RuntimeCellAdmission`.

Consumer: `prepare_cell_program_admitted`
(`tidepool/runtime/src/session/turn.rs:3110`) and the item path
`PersistentSession::begin_cell_program` (`admission.rs:2661`),
`admit_checked_item` (`admission.rs:2150`), and
`CheckedTurnCompletion::settle` (`admission.rs:1554`).

Must not carry:

- No independently issued checked output, no `CellProgram`, no
  `ExactCompiledItem`, no native permission. The admission carries expectations
  only; the service issuer supplies checked output.
- No finite view, catalog, interfaces, native imports, compile inputs,
  include paths, authority digest, or baseline. Those fields belong only to
  `RuntimeFiniteCellInputs` and must never be populated for a `World` basis.
- No caller-supplied reservation or slot list. The reservation must come from
  `CompilerWorldCellPlan::reserve` after `issue_admission_token`; a caller-built
  `CompilerWorldCellReservation` must not be accepted.
- No target B as original A, and no original A reconstructed from labels or
  digests. The two leases are distinct fields and must stay distinct.
- No re-derivation or widening of `reserved_generations`,
  `initial_value_generation`, `slots`, or `expected_bindings` after issue. The
  `AdmissionDigest` (`world_cell_admission_digest`, `cell_plan.rs:423`) binds
  the exact owner/epoch/session/scope, world logical id, producer, plan digest,
  slots and binders.
- No second transaction standing in for the issuing source. The reservation is
  valid only inside the source transaction whose origin
  `WorldCellAdmissionSubject::for_source_transaction` observed.

### 1.4 Original source A vs target/private placement B

Types and owners:

- `tidepool_runtime::session::inspection::InspectionWorldBase`
  (`tidepool/runtime/src/session/inspection.rs:531`) — lifecycle.
- `tidepool_runtime::session::PrivateExecutionAdmission` (`admission.rs:168`) —
  runtime-publication.
- `tidepool_runtime::session::PrivateExecutionCompilerBasis` (`admission.rs:184`)
  — runtime-publication.
- `tidepool_runtime::session::WorldExecutionAdmissionFacts` (`admission.rs:205`)
  — runtime-publication.
- `tidepool_runtime::session::source_admission::RuntimeSourceCompilerTransaction`
  (`tidepool/runtime/src/session/source_admission.rs:792`) — lifecycle.
- `tidepool_toolchain::compiler_world::WorldSourceSelection`
  (`tidepool/toolchain/src/compiler_world.rs:2109`) — service-cells.

`InspectionWorldBase` fields (`inspection.rs:531`):

- `scope: ScopeId` — `exists-today`.
- `key: CompileViewKey` — `exists-today`.
- `owner: Arc<RuntimeAdmissionOwner>` — `exists-today`.
- `epoch: u64` — `exists-today`.
- `source: Arc<GenerationArtifactReference>` — `exists-today`. The published
  logical source selection.
- `original_source: Arc<RuntimeLexicalScopeLease>` — `exists-today`. Original A.
- `target: Arc<RuntimeLexicalScopeLease>` — `exists-today`. Target B.

`RuntimeSourceCompilerTransaction` fields (`source_admission.rs:792`):

- `source: Arc<RuntimeLexicalScopeLease>` — `exists-today`. The issuing source
  transaction's A.
- `origin: CompilerTransactionOrigin` — `exists-today`.
- `hook: Option<SourceAdmissionHook>` — `exists-today`.
- `holds: Mutex<SourceCompilerHolds>` — `exists-today`.
- `closed: AtomicBool` — `exists-today`.

`WorldExecutionAdmissionFacts` fields (`admission.rs:205`):

- `published: Arc<InspectionWorldAssociation>` — `exists-today`.
- `source: Arc<GenerationArtifactReference>` — `exists-today`.
- `original_source: Arc<RuntimeLexicalScopeLease>` — `exists-today`. A.
- `admitted: PublicVisibilitySnapshot` — `exists-today`.
- `admitted_context: Option<Arc<ExactDeclarationContext>>` — `exists-today`.
- `admitted_public_view: [u8; 32]` — `exists-today`.

`PrivateExecutionCompilerBasis::World` fields (`admission.rs:184`):

- `published`, `private_key`, `source`, `original_source`, `admitted`,
  `admitted_context`, `admitted_public_view` — all `exists-today`.

`RuntimeCellCompilerBasis::World.original_source` (`admission.rs:673`) —
`exists-today`. R1 adds only an accessor on the private execution:
`PrivateExecutionAdmission::original_source()` (`admission.rs:599` (R1)).

`RuntimeCellAdmission::source_lease()` (`admission.rs:1865`) — `exists-today`;
returns `_retained_scope`, which is target B. `added-by-R1`: rename or pair it
with `RuntimeCellAdmission::original_source()`; the base does not expose A from
the cell admission. `removed`: any use of `source_lease()` as original A.

`WorldSourceSelection` fields (`compiler_world.rs:2109`):

- `sha256: [u8; 32]` — `exists-today`.
- `revision: String` — `exists-today`.
- `public_root: ExactModuleIdentity` — `exists-today`.

Constructor:

- A: lifecycle's `RuntimeSourceCompilerTransaction` captures the issuing
  source in `admit`/`origin` (`source_admission.rs:805-810`); R1's
  `begin_private_execution` (`admission.rs:2913` (R1)) reads
  `source_transaction.source` and `begin_world_private_execution`
  (`admission.rs:3004` (R1)) checks it against `InspectionWorldBase`.
- B: `begin_private_execution_with_world` (`admission.rs:3051` (R1)) mints the
  detached private scope and captures `prepared_lexical_scope_lease(private_scope)`
  (`admission.rs:3080` (R1)).
- The world source pair is validated by `inspection_world_base_is_current`
  (`persistent.rs:1766`) and by `begin_world_private_execution`'s
  `Arc::ptr_eq(&transaction.source, original_source)` (`admission.rs:3014`
  (R1)).

Consumer:

- R1's `reserve_world_cell` (`admission.rs:3958` (R1)) checks
  `Arc::ptr_eq(&transaction.source, &original_source)` (`:3977`) and stores A in
  `RuntimeCellCompilerBasis::World.original_source`.
- `prepare_cell_program_admitted` (`turn.rs:3110`) calls
  `source.admit(Some(admission.source_lease()))` (`turn.rs:3114` (R1)) — this is
  B, and is correct only as the target claim. It must never be read as A.

Must not carry:

- No caller's current transaction standing in for A. `begin_world_private_execution`
  rejects `ForeignTransaction`; `reserve_world_cell` repeats the check.
- No B substituted for A. `source_lease()` is B by construction; the cell
  admission must expose A separately.
- No A reconstructed from source labels, `WorldSourceSelection` plain facts,
  `GenerationArtifactReference`, or a caller's `CompileViewKey`.
- No `WorldSourceSelection` pair assembled by an arbitrary caller. The public
  `WorldSourceSelection::new` (`compiler_world.rs:2115`) is shape-only and must
  become transaction-bound or crate-private (D3).
- No equality collapse: `CheckedWorldCellProgram::admit` refuses
  `original == target` (`compiler_world.rs:2340`); A and B must remain distinct
  even when their bytes match.

### 1.5 `CellProgram` (executable) vs `CheckedWorldCellProgram` (certified world output)

Types and owners:

- `tidepool_toolchain::compiler_world::CheckedWorldCellProgram`
  (`tidepool/toolchain/src/compiler_world.rs:2231`) — service-cells.
- `tidepool_toolchain::compiler_world::CheckedWorldCellItem`
  (`compiler_world.rs:2176`) — service-cells.
- `tidepool_toolchain::compiler_world::WorldCellLogicalIdentity`
  (`compiler_world.rs:2152`) — service-cells.
- `tidepool_toolchain::compiler_world::WorldCellOutputAdmission`
  (`compiler_world.rs:1987`, private) — service-cells.
- `tidepool_toolchain::compiler_world::WorldItemProof`
  (`compiler_world.rs:2058`, `pub(crate)`) — service-cells.
- `tidepool_toolchain::checked_cell::CellProgram`
  (`tidepool/toolchain/src/checked_cell.rs:1078`) — service-cells.
- `tidepool_toolchain::checked_cell::CellProgramItem`
  (`checked_cell.rs:1086`) — service-cells.
- `tidepool_toolchain::checked_cell::CellProgramObservations`
  (`checked_cell.rs:1092`, `pub(crate)`) — service-cells.

`CheckedWorldCellProgram` fields (`compiler_world.rs:2231`):

- `output: WorldCellOutputAdmission` — `exists-today` (private).
- `original: WorldSourceSelection` — `exists-today`.
- `target: WorldSourceSelection` — `exists-today`.
- `logical: WorldCellLogicalIdentity` — `exists-today`.
- `items: Vec<CheckedWorldCellItem>` — `exists-today`.
- `certificate_sha256: [u8; 32]` — `exists-today`.

`CheckedWorldCellItem` fields (`compiler_world.rs:2176`):

- `ordinal: usize` — `exists-today`.
- `generation: u64` — `exists-today`.
- `entry: String` — `exists-today`.
- `native_ordinal: u32` — `exists-today`.
- `segment_digest: String` — `exists-today`.
- `symbol: SymbolIdentity` — `exists-today`.
- `manifest: BTreeMap<PathBuf, [u8; 32]>` — `exists-today`.
- `binders: Vec<WorldItemBinder>` — `exists-today` (`pub(crate)` accessor).
- `signatures: Vec<ExactCheckedSignature>` — `exists-today` (`pub(crate)` accessor).

`WorldCellOutputAdmission` fields (`compiler_world.rs:1987`, private):

- `origin`, `reservation`, `directory`, `observations`, `items`, `common_files`,
  `demanded_rows`, `captured_files`, `validation`, `segments`, `item_proofs` —
  all `exists-today`.

`CellProgram` fields (`checked_cell.rs:1078`):

- `checked: Arc<ExactCheckedCell>` — `exists-today` (`pub(crate)`).
- `parsed: Arc<ParsedCellPlan>` — `exists-today` (`pub(crate)`).
- `slots: Vec<CheckedPlannedCellSlot>` — `exists-today` (`pub(crate)`).
- `items: Vec<CellProgramItem>` — `exists-today` (`pub(crate)`).

`CellProgramItem` fields (`checked_cell.rs:1086`):

- `checked: ExactCheckedItem` — `exists-today` (`pub(crate)`).
- `native: Option<Arc<ExactCompiledItem>>` — `exists-today` (`pub(crate)`).
- `native_observations: Option<CellProgramObservations>` — `exists-today`
  (`pub(crate)`).

Constructor:

- `CheckedWorldCellProgram`: `admit_world_cell_program`
  (`compiler_world.rs:2526`) via private `CheckedWorldCellProgram::admit`
  (`compiler_world.rs:2339`). Today it has no production caller.
- `CellProgram`: `PreparedCellProgram::seal` (`artifacts.rs:744`) only. S1 must
  add one world-output path that reaches the same seal (for example a
  `PreparedCellProgram::from_world` constructor owned by service-cells) without
  creating a second seal.

Consumer:

- `CheckedWorldCellProgram` is service-internal and is consumed by the
  service's own output issuance
  (`prepare_world_cell_outputs`, `compiler_world.rs:2539`, and
  `decode_world_source_receipt`, `declaration_context.rs:8975`). It must not
  cross to R1 as execution authority.
- `CellProgram` is the runtime-facing executable value, consumed by
  `begin_cell_program` (`admission.rs:2661`), `admit_checked_item`
  (`admission.rs:2150`), and `finish_cell_program_admitted` (`turn.rs:3126`).

Must not carry:

- `CheckedWorldCellProgram` must not carry or become runtime execution
  authority. It is a certified world output: reservation, original/target,
  logical identity, per-item generations and certificate. It does not carry
  the evaluator's executable items.
- `CheckedWorldCellItem` is not `ExactCheckedItem` and is not
  `ExactCompiledItem`. It carries no prepared target, native entry, value
  interface, settled values, or successor description.
- `CellProgram` must not carry raw completion bytes, a raw world receipt, a
  caller-built checked item, or a second certificate. It must be issued exactly
  once from the matching reservation/output.
- No second certifier and no parallel `WorldCellProgram` evaluator type. Extend
  the existing `CellProgram`/`ExactCompiledItem` owners.

### 1.6 `PreparedCheckedItemOutput`, `ExactCheckedItem`, `ExactCompiledItem`

Types and owners:

- `tidepool_toolchain::checked_cell::PreparedCheckedItemOutput`
  (`tidepool/toolchain/src/checked_cell.rs:3993`, `pub(crate)`) — service-cells.
- `tidepool_toolchain::checked_cell::ExactCheckedItem`
  (`checked_cell.rs:2634`) — service-cells.
- `tidepool_toolchain::checked_cell::ExactCompiledItem`
  (`checked_cell.rs:2876`) — service-cells.
- `tidepool_toolchain::checked_cell::CheckedItemOffer`
  (`checked_cell.rs:3905`-ish, `pub(crate)`) — service-cells.
- `tidepool_toolchain::checked_cell::CheckedExecutionAdmission`
  (`checked_cell.rs:2868`, private) — service-cells.

`PreparedCheckedItemOutput` fields (`checked_cell.rs:3993`):

- `item: ExactCheckedItem` — `exists-today`.
- `generation: u64` — `exists-today`.
- `observation_name: Option<String>` — `exists-today`.
- `is_program: bool` — `exists-today`.
- `original_execution: Arc<ExactDeclarationContext>` — `exists-today`.
- `target: Arc<PreparedProgram>` — `exists-today`.
- `table: DataConTable` — `exists-today`.
- `warnings: MetaWarnings` — `exists-today`.
- `bound_binders: Vec<Value>` — `exists-today`.
- `authenticated_sites: [u8; 32]` — `exists-today`.
- `typed_entry: Option<SealedTypedEntry>` — `exists-today`.
- `source_originals: Vec<Arc<PublishedSourceOriginalSelection>>` —
  `exists-today`.
- `has_interface: bool` — `exists-today`.

`ExactCheckedItem` fields (`checked_cell.rs:2634`):

- `cell: Arc<ExactCheckedCell>` — `exists-today` (private).
- `index: usize` — `exists-today` (private).

Constructor: `ExactCheckedCell::item` (`checked_cell.rs:2601`) constructs
`ExactCheckedItem` at `checked_cell.rs:2606`. S1's world issuer must obtain the
exact checked item from the matching `ExactCheckedCell`, not synthesize an index
or a cell.

`ExactCompiledItem` fields (`checked_cell.rs:2876`):

- `original_execution: Arc<ExactDeclarationContext>` — `exists-today`.
- `item: ExactCheckedItem` — `exists-today`.
- `target: Arc<PreparedProgram>` — `exists-today`.
- `table: DataConTable` — `exists-today`.
- `warnings: MetaWarnings` — `exists-today`.
- `yield_sites_digest: [u8; 32]` — `exists-today`.
- `value_interface: Option<Arc<CheckedValueArtifact>>` — `exists-today`.
- `generation: u64` — `exists-today`.
- `bound_binders: Vec<Value>` — `exists-today`.
- `observation_name: Option<String>` — `exists-today`.
- `admission: CheckedExecutionAdmission` — `exists-today`. Either
  `RuntimeItem([u8; 32])` or `CellProgram([u8; 32])`.
- `settled_values: CheckedSettledValues` — `exists-today`.
- `typed_entry: Option<SealedTypedEntry>` — `exists-today`.
- `source_originals: Vec<Arc<PublishedSourceOriginalSelection>>` —
  `exists-today`.
- `successor: CellSuccessorDescription` — `added-by-S1`; see 1.8. It must be
  present for world items; finite items may omit it only if the finite route is
  still live.

Constructor: `PreparedCheckedItemOutput::capture`
(`checked_cell.rs:4016`) captures pre-close observations;
`CheckedItemOffer::seal_prepared` (`checked_cell.rs:3920`) validates that the
prepared output matches the offer and constructs `ExactCompiledItem` at
`checked_cell.rs:3965`. S1 owns both constructors. R1 must not call `capture`
or `seal_prepared`.

Consumer: `PreparedCellProgram::seal` (`artifacts.rs:744`) consumes
`PreparedCheckedItemOutput` and builds `CellProgramItem.native`; runtime
`begin_cell_program`/`admit_checked_item`/`settle` consume the sealed
`CellProgram`/`ExactCompiledItem`.

Must not carry:

- No independently assembled evidence constructor. `ExactCompiledItem` must not
  be built by runtime or tests; `PreparedCheckedItemOutput` must not cross the
  toolchain crate boundary.
- No raw receipt, raw completion, decoded transport bytes, or caller-supplied
  target/table/binders. `seal_prepared` checks the item identity, generation,
  observation name and program flag against the offer
  (`checked_cell.rs:3928-3935`).
- No successor description inferred from names, successful evaluation, or
  inventory scans. The successor is issued by the service before the universe
  strips the target.
- No finite baseline, `ExactCompilationRequest`, or caller-supplied source
  support. The world issuer must consume `DemandedWorldCompilerInputs` (1.7).
- No second seal. `PreparedCheckedItemOutput` is pre-close and not executable;
  only `PreparedCellProgram::seal` makes `CellProgram`.

### 1.7 Demanded execution evidence

Types and owners:

- `tidepool_toolchain::declaration_context::DemandNativeInputs`
  (`tidepool/toolchain/src/declaration_context.rs:2037`, `pub(crate)`) —
  service-cells.
- `tidepool_toolchain::declaration_context::DemandedWorldCompilerInputs`
  (`declaration_context.rs:6982`, `pub(crate)`) — service-cells.
- `tidepool_toolchain::declaration_context::ProductInputFacts`
  (`declaration_context.rs:6910`, private) — service-cells.
- `tidepool_toolchain::declaration_context::DemandCertifiedGroups` — service-cells.
- `tidepool_toolchain::checked_cell::CheckedValueImportAuthority`
  (`checked_cell.rs:1892`, `pub(crate)`) — service-cells.
- `tidepool_toolchain::compiler_world::WorldCellOutputAdmission.demanded_rows`
  (`compiler_world.rs:1997`, private) — service-cells.
- `tidepool_toolchain::checked_cell::ExactCompiledItem.value_interface`,
  `.typed_entry`, `.source_originals`, `.settled_values` — `exists-today`.

`DemandNativeInputs` fields (`declaration_context.rs:2037`):

- `cold: Arc<RequestCompilerInputs>` — `exists-today`.
- `demand: DemandCompilerProjection` — `exists-today`.
- `native_selection: NativeInputSelection` — `exists-today`.
- `permission_delta: IssuedNativePermissionDelta` — `exists-today`.
- `work: DemandNativeInputsWork` — `exists-today`.

`DemandedWorldCompilerInputs` fields (`declaration_context.rs:6982`):

- `demand: DemandNativeInputs` — `exists-today`.
- `groups: DemandCertifiedGroups` — `exists-today`.
- `support: Arc<CompilerSourceSupport>` — `exists-today`.
- `binding: WorldCompilationIdentity` — `exists-today`.
- `context: &Arc<ExactDeclarationContext>` — `exists-today`.
- `checked_values: CheckedValueImportAuthority` — `exists-today`.

`ProductInputFacts::from_demand` (`declaration_context.rs:6947`) currently
supplies:

- `source_lexical: &[]` — `exists-today`; incomplete placeholder.
- `program_support: None` — `exists-today`.
- `source_selected_support: &EMPTY_SELECTED_SUPPORT` — `exists-today`;
  incomplete placeholder.
- `source_search_include: None` — `exists-today`.
- `scaffold_imports: &[]` — `exists-today`; incomplete placeholder.

`demand_primary_inputs` (`declaration_context.rs:8849`) currently reads
`cold.source_support.projection().snapshot_roles()`
(`declaration_context.rs:8855`) — a full role census, not the demanded closure.

Constructor:

- `DecodedWorldSourceReceipt::demanded_product_inputs`
  (`declaration_context.rs:8948`) runs
  `demand_primary_inputs` → `certify_receipt_groups` →
  `semantic_source_support` and constructs `DemandedWorldCompilerInputs`.
- `ProductInputFacts::from_demand` (`declaration_context.rs:6947`) assembles
  the product facts.
- `CheckedValueImportAuthority::from_world_output`
  (`checked_cell.rs:1901`) borrows only the exact rows selected by the prepared
  program.

Consumer: S1's world output issuer (`prepare_world_cell_outputs`,
`compiler_world.rs:2539`) and `PreparedCheckedItemOutput::capture`
(`checked_cell.rs:4016`). Today `DemandedWorldCompilerInputs` is constructed but
unadopted; S1 must adopt it as the only demanded closure for world output.

Must not carry:

- No full `snapshot_roles()` census or broad inventory. The plan requires keyed
  owner lookup from the exact demanded closure; a narrow returned view must not
  hide a broad scan.
- No finite `ExactCompilationRequest`, `CheckedCellRequest` baseline, catalog
  selection, or all-value interface capture.
- No empty placeholders standing in for real lexical/scaffold obligations.
  `from_demand`'s empty `source_lexical`/`scaffold_imports` are incomplete
  seams, not evidence that the obligations are absent.
- No caller-selected rows or a second resolver. Native selection owns exact
  bindings; service cells own the demanded lexical/type/instance/scaffold
  obligations.
- No `ExactCheckedItem`/`ExactCompiledItem` constructed from a successful
  evaluation or a raw receipt.

### 1.8 Item-bound private successor descriptions

Type: `tidepool_toolchain::checked_cell::CellSuccessorDescription` —
`added-by-S1`, service-cells. It does not exist today. S1 must add it in
`checked_cell.rs` (or the owning world-output module) and carry it on
`ExactCompiledItem.successor` (1.6). R1 must not define a parallel successor
type.

Required variants and fields (exact names are frozen here; private helper types
may be added inside the owner):

- `BindOrObservation`
  - `ordinal: usize` — `added-by-S1`.
  - `occurrence: String` — `added-by-S1`. Actual accepted occurrence.
  - `symbol: SymbolIdentity` — `added-by-S1`. Full compiler-issued symbol.
  - `session_var: SessionVarId` — `added-by-S1`.
  - `defining_scope: u64` — `added-by-S1`.
  - `generation: u64` — `added-by-S1`.
  - `row: Arc<CheckedValueArtifact>` — `added-by-S1`. Exact checked row.
  - `native_delta: SuccessorNativeDelta` — `added-by-S1`. Captured native
    delta for this item only.
- `DeclarationOrImport`
  - `ordinal: usize` — `added-by-S1`.
  - `join: AcceptedDeclarationJoin` — `added-by-S1`. Accepted checked
    declaration/join (the exact existing owner type, not a copy).
  - `lexical_changes: SuccessorLexicalChanges` — `added-by-S1`.
  - `source_changes: SuccessorSourceChanges` — `added-by-S1`.
  - `roles: SuccessorRoleChanges` — `added-by-S1`.
- `ShadowOrRemoval`
  - `ordinal: usize` — `added-by-S1`.
  - `prior: CompleteBindingFact` — `added-by-S1`. The expected complete old
    binding fact.
  - `replacement: Option<CompleteBindingFact>` — `added-by-S1`. Actual
    replacement, or `None` for deletion.

Constructor: the service issuer, before the reusable compiler universe strips
the target. S1 must construct it in the same path that produces
`PreparedCheckedItemOutput`/`ExactCompiledItem` (for example inside
`PreparedCellProgram::seal`'s world branch or a dedicated world-output issuer),
using the item's own successful output.

Consumer: R1's `CheckedTurnCompletion::settle`
(`tidepool/runtime/src/session/admission.rs:1554`), which combines the
successor description with only that item's actual runtime settlement delta.
No other consumer may derive or widen it.

Must not carry:

- No ancestor world chain, disposable operation directory, reservation owner,
  or whole-cell baseline. A successor must not own an ancestor chain merely to
  preserve reuse (service-cells plan:165-166).
- No names, inventory, or successful-evaluation scan used to infer compiler
  roles, native choices, or dependency edges.
- No later prepared item's visibility. Each description binds its actual
  ordinal and predecessor prefix only.
- No second finalizer or public adoption authority. The runtime completion
  assembles already issued facts; it does not issue compiler authority.
- No caller-supplied old/new facts for shadow/removal. The expected prior fact
  must come from the issuing owner.

Today's gap: no Rust successor-description type exists.
`ExactCompiledItem` carries `settled_values`, `typed_entry`, `value_interface`,
and `source_originals`, but not an item-bound successor. Haskell
`retainHeldWorldSuccessor` (`bridge/haskell/src/Tidepool/GhcPipeline.hs:1621`)
is definition-only; `validateHeldWorldCell` (`GhcPipeline.hs:1992`) excludes
declarations and multiple inference segments (`GhcPipeline.hs:2005-2006`), and
`compileHeldWorldCell` (`GhcPipeline.hs:2019`) refuses checked-value home
requirements (`GhcPipeline.hs:2044`).

### 1.9 `PreparedRuntimeCell` and `finish_cell_program_admitted` sealing

Types and owners:

- `tidepool_runtime::session::turn::PreparedRuntimeCell`
  (`tidepool/runtime/src/session/turn.rs:3079`) — runtime-publication.
- `tidepool_runtime::session::turn::finish_cell_program_admitted`
  (`turn.rs:3126`) — runtime-publication.
- `tidepool_toolchain::artifacts::PreparedCellProgram`
  (`tidepool/toolchain/src/artifacts.rs:707`) — service-cells.
- `tidepool_toolchain::artifacts::PreparedCellProgram::seal`
  (`artifacts.rs:744`) — service-cells.

`PreparedRuntimeCell` fields (`turn.rs:3079`, all private):

- `origin: CompilerTransactionOrigin` — `exists-today`.
- `checked: CellCheck` — `exists-today`.
- `program: PreparedCellProgram` — `exists-today`.
- `offer: AppliedModuleCandidateOffer` — `exists-today`.
- `root: Arc<TempDir>` — `exists-today`.
- `stderr: Vec<u8>` — `exists-today`.

`PreparedCellProgram` fields (`artifacts.rs:707`, all private):

- `root_owner`, `checked`, `planned`, `context`, `source_lexical`,
  `produced_outputs`, `outputs`, `prepared_items`, `pending_segments`,
  `validation` — all `exists-today`.

Constructor: `prepare_cell_program_admitted` (`turn.rs:3110`) →
`prepare_cell_program_admitted_inner` (`turn.rs:3291`) constructs
`PreparedRuntimeCell` at `turn.rs:3480`. S1 must provide the world equivalent of
`PreparedCellProgram` (or a `PreparedCellProgram::from_world` constructor) so the
runtime can construct `PreparedRuntimeCell` without a second seal.

Consumer: `finish_cell_program_admitted` (`turn.rs:3126`) →
`PreparedRuntimeCell::seal` (`turn.rs:3189`) →
`PreparedCellProgram::seal` (`artifacts.rs:744`) → `Arc<CellProgram>`.

Exact seal rules (freeze these):

- `finish_cell_program_admitted` checks `outcome.belongs_to(&prepared.origin)`
  (`turn.rs:3152-3157`); a product from another compiler transaction is
  `DifferentOwner`.
- It refuses `CompilerTransactionClose::Unconfirmed` and `::Retired`
  (`turn.rs:3146-3164`); only `Clean` or `NotStarted` may seal.
- It requires `host_admission_cancellation` (`turn.rs:3166-3170`); a missing
  cancellation owner is `MissingOwner`.
- `PreparedCellProgram::seal` checkpoints cancellation before and after each
  item and before publishing pending segments (`artifacts.rs:748, 762, 833,
  838, 843`). A cancelled checkpoint returns `Interrupted` and publishes
  nothing.
- The seal returns `Arc<CellProgram>` exactly once.

Must not carry:

- No second seal and no second finalizer. `PreparedRuntimeCell` is the runtime's
  prepared owner; `PreparedCellProgram::seal` is the only executable seal.
- No sealing before a clean/`NotStarted` close, no sealing after cancellation,
  and no publication on `Unconfirmed`/`Retired`.
- No caller-supplied close, origin, owner, cancellation, or checked program.
- No world output that bypasses the matching settled transaction or the
  `belongs_to_owner` check.
- No finite `RuntimeFiniteCellInputs` used to build a world `PreparedRuntimeCell`.

### 1.10 Cancellation and END seal

Types and owners:

- `tidepool_extract_cmd::endpoint::CompilerTransactionClose`
  (`tidepool/extract-cmd/src/endpoint.rs:343`) — service-cells (compiler
  client/wire), consumed by runtime-publication.
- `tidepool_extract_cmd::endpoint::CompilerTransactionSettled<T>`
  (`endpoint.rs:277`) — service-cells.
- `tidepool_extract_cmd::endpoint::CompilerTransactionCancellation`
  (`endpoint.rs:967`) — service-cells.
- `tidepool_extract_cmd::endpoint::HostAdmissionCancellation`
  (`endpoint.rs:975`) — service-cells.

`CompilerTransactionClose` variants (`endpoint.rs:343`):

- `NotStarted` — `exists-today`.
- `Clean` — `exists-today`.
- `Retired(CompilerTransactionCloseEvidence)` — `exists-today`.
- `Unconfirmed(CompilerTransactionCloseEvidence)` — `exists-today`.

`CompilerTransactionSettled<T>` fields (`endpoint.rs:277`, private):

- `outcome: CompilerTransactionOutcome<T>` — `exists-today`.
- `origin: CompilerTransactionOrigin` — `exists-today`.
- `host_admission: Option<HostAdmissionCancellation>` — `exists-today`.

`HostAdmissionCancellation` fields (`endpoint.rs:975`):

- `cancelled: Arc<AtomicBool>` — `exists-today`.

Methods: `checkpoint` (`endpoint.rs:1003`) and `run_sync`
(`endpoint.rs:1018`). `CompilerTransactionCancellation` (`endpoint.rs:967`)
owns the exact direct child/daemon connection for the scoped transaction.

Constructor: the compiler transaction runner constructs
`CompilerTransactionSettled` at `endpoint.rs:2379`; no caller may synthesize a
close.

Consumer: `finish_cell_program_admitted` (`turn.rs:3126`) and
`PreparedCellProgram::seal` (`artifacts.rs:744`).

Must not carry:

- No unconfirmed END treated as success. `Unconfirmed` retains owners and
  refuses execution/publication; it is never a reason to retry cold.
- No `Retired` END treated as success. `Retired` refuses execution/publication.
- No cancellation after seal, and no publication after a cancelled checkpoint.
- No close from another owner. `belongs_to_owner` compares the exact
  `CompilerTransactionOrigin`.
- No caller-created `Clean`/`NotStarted`. Only the transaction runner issues the
  settled product.

## 2. Divergence report: R1 draft vs S1 draft

The S1 tree is clean at `7f88d89afb`; its draft is the plan. The divergences
below are therefore R1's dirty code against S1's plan, plus places where the
plan is insufficient to prevent an authority leak.

### D1. Base drift: both owners are based on `7f88d89afb`, not the freeze base

- S1 is clean at `7f88d89afb`; R1 committed through `64a5573930`, but that
  history still branches from `7f88d89afb`. The freeze base is `e36794e6f9`
  (`bba9588b69`, `638b0a3daf`, `e36794e6f9`).
- `7f88..e367` changes `bridge/haskell/{app/Main.hs,src/Tidepool/CompilerRequest.hs,src/Tidepool/ExactScope.hs,src/Tidepool/GhcPipeline.hs}`
  and Haskell tests; no Rust files. Rust line numbers remain valid, but S1's
  Haskell request/completion/pipeline work is built on the older tree.
- Resolution: both owners rebase onto `e36794e6f9` before adding types. S1
  re-reads `GhcPipeline.hs` at `e367` because the Q2/Q3 admitted-operation
  changes touch the same request/completion path.

### D2. `source_lease()` is target B; the cell admission has no A accessor

- Base: `RuntimeCellAdmission::source_lease()` (`admission.rs:1865`) returns
  `_retained_scope`, which is target B. `RuntimeCellCompilerBasis::World` stores
  A in `original_source` (`admission.rs:673`) with a temporary `dead_code`
  allowance.
- R1 adds `PrivateExecutionAdmission::original_source()` (`admission.rs:599`
  (R1)) but not `RuntimeCellAdmission::original_source()`. Consequently
  `prepare_cell_program_admitted` still calls
  `source.admit(Some(admission.source_lease()))` (`turn.rs:3114` (R1)), which is
  B, and there is no cell-level A accessor for the world issuer.
- Resolution: add
  `RuntimeCellAdmission::original_source() -> Arc<RuntimeLexicalScopeLease>`
  returning the stored A, and rename `source_lease()` to `target_source()` (or
  add `target_source()` and delete `source_lease()`). Update
  `prepare_cell_program_admitted` to use `target_source()` explicitly. In R1's
  `admit_world_planned_cell_for_execution` (`admission.rs:3830-3834` (R1)),
  use the single `execution.original_source()` accessor instead of reaching
  into `published.base.original_source` a second way.

### D3. `WorldSourceSelection::new` is public and shape-only

- `WorldSourceSelection::new` (`compiler_world.rs:2115`) is public and checks
  only `sha256 != 0`, nonempty revision/public root. `admit_world_cell_program`
  (`compiler_world.rs:2526`) accepts caller-supplied `original` and `target`.
- The contract requires A/B captured from their real owners
  (contracts.md: "Keep issuing source A distinct from receiving target B, both
  captured from their real owners. A caller's current transaction cannot
  substitute for the original A."). A caller can currently assemble both.
- Resolution: make `WorldSourceSelection::new` `pub(crate)` and replace it with
  a transaction-bound constructor (the `WorldCellAdmissionSubject` pattern:
  check `CompilerTransactionOrigin::current() == origin`), or have
  `admit_world_cell_program` consume an opaque source-pair receipt issued by the
  lifecycle source owner. S1 must not accept raw A/B selections from R1.

### D4. R1 refuses every `Expression`; S1 requires expressions and a collision witness

- R1: `reserve_world_cell` returns
  `WorldCellExpressionReservationUnsupported` for the first `Kind::Expression`
  item (`admission.rs:3990-4000` (R1)) and again per item
  (`admission.rs:4083-4091` (R1)).
- S1 plan: support expression observations as ordinary items
  (service-cells plan:108-112, "bindings, expression observations, and mixed
  cells"; runtime-publication plan:69-70, "Expression observation names require
  the service's maintained collision witness").
- The base finite path derives `observation{capture}` plus a `names` set at
  `admission.rs:3465`; the world plan has no witness today.
- Resolution: S1 adds the service-issued `WorldObservationName` (1.2) to the
  parser plan/receipt; R1 reserves that exact name, verifies its
  `world_logical_id` against the reservation world, and removes both blanket
  refusals. The runtime must not derive or enumerate names.

### D5. No checked-world → executable issuer; R1 refuses before compiler work

- `CheckedWorldCellProgram` (`compiler_world.rs:2231`) carries the certified
  world output but no `CellProgram`/`ExactCompiledItem` path;
  `admit_world_cell_program` (`compiler_world.rs:2526`) has no production
  caller (completion/plan.md:36-38).
- R1 refuses any world reservation in the preparer
  (`turn.rs:3308-3319` (R1), `WorldCellCheckedProgramCapabilityMissing`), and
  base `begin_cell_program`/`begin_cell_program_prefix` refuse at
  `admission.rs:2673` and `:2724`.
- S1 plan says "Wire `admit_world_cell_program` into the existing product
  issuance chain" (service-cells plan:83-90), but that is not yet code.
- Resolution: S1 wires the world output through the existing
  `PreparedCheckedItemOutput` → `CheckedItemOffer::seal_prepared` →
  `ExactCompiledItem` → `CellProgram` chain, and provides the
  `PreparedCellProgram`/`PreparedRuntimeCell` integration. R1 deletes the
  capability refusals once the issuer lands. Until then, R1's refusal is a
  correct temporary guard, not the final contract.

### D6. No item-bound successor description; R1 refuses settlement

- No Rust successor-description type exists. `ExactCompiledItem`
  (`checked_cell.rs:2876`) has no successor field; `CellProgramItem`
  (`checked_cell.rs:1086`) has only checked/native/observations.
- R1/base refuse world settlement with `WorldCellSuccessorCapabilityMissing`
  at `admission.rs:1618-1626` (`CheckedTurnCompletion::settle`),
  `admission.rs:2181-2185` (`admit_checked_item`), and `admission.rs:2280`
  (declaration adoption).
- S1 plan requires the description (service-cells plan:144-160;
  runtime-publication plan:87-93).
- Resolution: S1 adds `CellSuccessorDescription` and
  `ExactCompiledItem.successor` (1.8), and constructs it from the item's own
  successful output before the universe strips the target. R1's `settle`
  consumes it with the same item's runtime delta and removes the
  `WorldCellSuccessorCapabilityMissing` refusals.

### D7. Seal path mismatch: `PreparedRuntimeCell` is finite-only today

- `finish_cell_program_admitted` (`turn.rs:3126`) consumes
  `CompilerTransactionSettled<Result<PreparedRuntimeCell, CellCheckFailure>>`;
  `PreparedRuntimeCell` is constructed only by
  `prepare_cell_program_admitted_inner` (`turn.rs:3291`), which R1 makes refuse
  world reservations (`turn.rs:3308-3319` (R1)).
- S1 produces `CheckedWorldCellProgram`, not `PreparedRuntimeCell`.
- Resolution: S1 adds the world-owned `PreparedCellProgram` constructor (for
  example `PreparedCellProgram::from_world`) and R1 constructs
  `PreparedRuntimeCell` from it, so `finish_cell_program_admitted` remains the
  sole seal. Do not add a second `seal` or a world-specific finalizer.

### D8. `begin_private_execution` becomes world-only before startup publishes one

- R1 changes `begin_private_execution` (`admission.rs:2913` (R1)) to require a
  published world and to call `begin_world_private_execution`
  (`admission.rs:3004` (R1)). Base
  `begin_private_execution_with_world(public_scope, None)` built a finite basis.
- `begin_ephemeral_private_execution` (`admission.rs:3120`) calls
  `begin_private_execution`; many unit tests and actor startup callers still
  use the finite route (for example `exomonad/actor/src/start.rs:1124`).
- Resolution: keep the finite branch reachable through one entry until the
  startup/host preparation owner publishes a world (the plan says initial
  startup/host preparation establishes a world through the existing production
  source/compiler owner). R1 must not make every `begin_private_execution`
  caller fail with `MissingPublishedCompilerWorld` before that owner lands.
  The boundary rule stays: `Finite` never enters `reserve_world_cell`; `World`
  never falls back to finite.

### D9. Demanded closure is a full census plus empty placeholders

- `demand_primary_inputs` (`declaration_context.rs:8849`) uses
  `snapshot_roles()` (`:8855`); `ProductInputFacts::from_demand`
  (`:6947`) supplies empty `source_lexical` and `scaffold_imports`
  (`:6965-6969`). `DemandedWorldCompilerInputs` (`:6982`) is constructed but
  unadopted.
- S1 plan requires the exact demanded lexical/type/instance/scaffold closure
  and keyed owner lookup (service-cells plan:113-125).
- Resolution: S1 adopts `DemandedWorldCompilerInputs` as the only world-product
  input, replaces `snapshot_roles()` with keyed owner lookup from the demand
  owner, and fills the lexical/scaffold/protected graph from the issuing owner.
  R1 must not pass a census or a finite baseline to fill the gaps.

### D10. Host-carrier one-`Bind` restriction can leak into private execution

- R1's `admit_world_planned_host_carrier_cell_in` (`admission.rs:3877` (R1))
  refuses anything other than exactly one `Bind` with the matching binder
  (`:3912-3930`). That is correct for the host carrier.
- `admit_world_planned_cell_for_execution` (`:3830` (R1)) has no such
  restriction; it must stay that way for ordinary private cells.
- Resolution: keep the shape check inside the host-carrier entry only. Add a
  control that the host carrier refuses a second item while private execution
  accepts a multi-item cell.

### D11. `WorldCellAdmissionSubject` still accepts caller-supplied owner facts

- `WorldCellAdmissionSubject::for_source_transaction` (`cell_plan.rs:245`)
  checks only the active `CompilerTransactionOrigin`; `new` (`:259`) checks
  only that `owner_identity` is nonzero. R1's `reserve_world_cell` supplies
  `owner_identity`, `owner_epoch`, `session`, `scope`, and `authorization` from
  its own fields (`admission.rs:4097-4107` (R1)).
- This is an independently assembled evidence constructor: a caller inside the
  transaction can frame a subject for a different scope/session/owner if it has
  those values. The plan says the reservation must bind the same
  plan/source/purpose/origin and must not let a caller-supplied transaction
  stand in for the issuing source. The origin check covers the transaction; it
  does not cover the owner facts.
- Resolution: derive owner/epoch/session/scope from the runtime admission owner
  inside the owning constructor (for example
  `RuntimeAdmissionOwner::world_cell_subject(scope, purpose)`), and make
  `for_source_transaction` take that owner capability rather than raw fields.
  At minimum, make `WorldCellAdmissionSubject::for_source_transaction`
  `pub(crate)` and construct it only inside `reserve_world_cell` from the
  `RuntimeCellAdmission` owner.

### D12. S1's wording "wire `admit_world_cell_program`" is insufficient alone

- The S1 plan says to wire `admit_world_cell_program` into the issuance chain
  (service-cells plan:83-90), but does not say to remove the public
  `WorldSourceSelection::new` path or the caller-supplied A/B inputs.
- Resolution: pair the wiring with D3: A/B must come from the lifecycle source
  owner's transaction-bound selections; the service must not accept raw
  `WorldSourceSelection` values from an arbitrary caller. Keep the
  `original == target` refusal (`compiler_world.rs:2340`) and the
  `belongs_to_owner`/close checks.

## 3. Owner checklists

Each owner applies its checklist in its own crate. Do not commit a shared-file
stub to create the types for the other side. The first owner to land a type
publishes the exact signature; the other owner consumes it.

### 3.1 R1 (runtime publication)

- [ ] Rebase onto `e36794e6f9`; re-read the Q2/Q3 Haskell request/completion
  changes before touching the world request path.
- [ ] Add `RuntimeCellAdmission::original_source()` returning the stored A; add
  `RuntimeCellAdmission::target_source()` and remove/rename `source_lease()` so
  no caller can use B as A. Update `prepare_cell_program_admitted` to call
  `source.admit(Some(admission.target_source()))`.
- [ ] In `admit_world_planned_cell_for_execution`, use the single
  `PrivateExecutionAdmission::original_source()` accessor; do not reach into
  `published.base.original_source` a second way.
- [ ] Keep the `Arc::ptr_eq(&transaction.source, &original_source)` check in
  `reserve_world_cell`; keep the `InspectionWorldBase` A/B liveness check in
  `begin_world_private_execution`.
- [ ] Keep the one-`Bind` shape check only in
  `admit_world_planned_host_carrier_cell_in`; do not copy it into ordinary
  private execution.
- [ ] When S1 lands `WorldObservationName`, reserve the exact name from the
  plan item, verify its `world_logical_id`, and remove both
  `WorldCellExpressionReservationUnsupported` refusals.
- [ ] When S1 lands the checked-world issuer, remove the
  `WorldCellCheckedProgramCapabilityMissing` refusal in the preparer and the
  base `begin_cell_program`/`begin_cell_program_prefix` refusals; consume the
  issued `CellProgram`.
- [ ] When S1 lands `CellSuccessorDescription`, consume it in
  `CheckedTurnCompletion::settle` with exactly the same item's runtime delta,
  and remove the `WorldCellSuccessorCapabilityMissing` refusals in `settle`,
  `admit_checked_item`, and declaration adoption.
- [ ] Keep `finish_cell_program_admitted` as the sole seal: `Clean`/`NotStarted`
  only, `belongs_to_owner`, cancellation checkpoint, then
  `PreparedRuntimeCell::seal`.
- [ ] Do not make `begin_private_execution` world-only until the startup/host
  preparation owner publishes a world; keep the finite branch reachable through
  one entry until then, and never let `Finite` enter `reserve_world_cell`.
- [ ] Do not construct `CellProgram`, `ExactCompiledItem`,
  `PreparedCheckedItemOutput`, or `CompilerWorldCellReservation` in R1.
- [ ] Compile the affected runtime/actor consumers and run the focused counted
  controls for A/B, cancellation, unconfirmed END, and the host-carrier shape.

### 3.2 S1 (service cells)

- [ ] Rebase onto `e36794e6f9`; re-read `GhcPipeline.hs` and
  `CompilerRequest.hs` after the Q2/Q3 changes.
- [ ] Add `WorldObservationName` in `cell_plan.rs` with `name`,
  `world_logical_id`, and `names_domain_sha256`; add
  `ParsedCellPlanItem.observation_name: Option<WorldObservationName>` for
  `Expression`; extend the parser receipt/plan version and the Haskell
  `encodeHeldWorldCellPlan` (`GhcPipeline.hs:1967`) to carry it. The service
  computes the name against its world occurrence table without enumerating
  historical names at the runtime boundary.
- [ ] Make `WorldSourceSelection::new` `pub(crate)` or transaction-bound; do
  not accept arbitrary A/B `WorldSourceSelection` values in
  `admit_world_cell_program`. Receive A/B from the lifecycle source owner.
- [ ] Wire `admit_world_cell_program` through the existing
  `PreparedCheckedItemOutput` → `CheckedItemOffer::seal_prepared` →
  `ExactCompiledItem` → `CellProgram` chain. Decode the SAME captured common
  receipt, item manifests, typed segments, and selected row references once
  under the existing `PackageInterfaceValidation`/inventory operation.
- [ ] Add the single world-owned `PreparedCellProgram` constructor (for example
  `PreparedCellProgram::from_world`) so R1 can build `PreparedRuntimeCell`
  without a second seal.
- [ ] Add `CellSuccessorDescription` and
  `ExactCompiledItem.successor`; construct it from the item's own successful
  output before the universe strips the target. Cover `BindOrObservation`,
  `DeclarationOrImport`, and `ShadowOrRemoval` with the fields in 1.8.
- [ ] Adopt `DemandedWorldCompilerInputs` as the only world-product input:
  replace `snapshot_roles()` with keyed owner lookup, fill
  `source_lexical`/`scaffold_imports`/protected graph from the issuing owner,
  and remove the empty placeholders in `ProductInputFacts::from_demand`.
- [ ] Remove the Haskell `validateHeldWorldCell` declaration and
  multiple-inference-segment exclusions (`GhcPipeline.hs:2005-2006`) and the
  `compileHeldWorldCell` checked-value home-requirement refusal
  (`GhcPipeline.hs:2044`) only as the real output/ordered-prefix controls
  compile and execute.
- [ ] Keep `PreparedCellProgram::seal` as the only executable seal; do not add
  a world-specific finalizer or a second certifier.
- [ ] Do not construct `RuntimeCellAdmission`, `RuntimeCellPurpose`,
  `PreparedRuntimeCell`, or any `RuntimeLexicalScopeLease`.
- [ ] Compile the production toolchain, Haskell worker/Tasty consumers, and the
  paired runtime/actor consumers; run the real Seed bind/import/expression/
  declaration/mixed-cell controls.

### 3.3 Lifecycle (A/B source owner)

- [ ] Preserve `InspectionWorldBase.original_source` (A) and
  `.target` (B) as distinct issued leases; expose only the exact accessors R1
  needs.
- [ ] Capture A once from the `RuntimeSourceCompilerTransaction.source`; reject
  a foreign transaction with `ForeignTransaction`.
- [ ] Provide the transaction-bound A/B `WorldSourceSelection` pair for
  `admit_world_cell_program`; do not let the service accept caller-built
  selections.
- [ ] Keep `source_lease()`/target B out of original-A identity checks; A and
  B may have equal bytes but must never be the same lease.
- [ ] Coordinate the shared `admission.rs`/`turn.rs` edits with R1; lifecycle
  sends focused changes for R1 to receive rather than rewriting the runtime
  constructors.

## 4. Wrong-draft items (highest priority)

1. **R1's blanket `Expression` refusal is wrong as a final contract.**
   `admission.rs:3990-4000` and `:4008-4013` (R1) reject every expression
   because the collision witness is missing. S1 requires expressions and owns
   the witness; keep the refusal only as a temporary guard, then reserve the
   service-issued exact name and delete both refusals.

2. **R1's `WorldCellCheckedProgramCapabilityMissing` refusal is temporary and
   must be removed when S1 lands the issuer.** `turn.rs:3308-3319` (R1) plus
   `admission.rs:2673`/`:2724` correctly stop a finite-baseline fallback today,
   but they are not the world delivery. The final path is the service-issued
   `CellProgram`; leaving the refusal after that ships would block the only
   supported route.

3. **S1's plan wording "wire `admit_world_cell_program`" is insufficient
   without closing the `WorldSourceSelection` leak.** The public shape-only
   `WorldSourceSelection::new` (`compiler_world.rs:2115`) and caller-supplied
   A/B arguments to `admit_world_cell_program` (`compiler_world.rs:2526`) let
   any caller assemble the source pair. Make the selections transaction-bound
   or crate-private and take them from the lifecycle source owner before
   claiming the wiring is complete.

Also note, but lower priority: R1's `begin_private_execution` becomes world-only
before startup publishes a world (D8). That is a real cutover ordering issue,
not a type-boundary ambiguity; resolve it with the startup/host preparation
owner rather than by adding a finite fallback to the world route.
