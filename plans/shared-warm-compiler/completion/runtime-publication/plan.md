# Sole actor cell route and atomic world publication

Status: implementation plan, not implementation or passing evidence. Read
[the campaign plan](../plan.md) and [locked contracts](../contracts.md) first.
Source audit: `6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`, code `92fa518105`.
Record the published plan and actual code OIDs at kickoff.

## Ownership and received contracts

The engine integrator receives this leaf. Own
`tidepool/runtime/src/session/{admission,turn,persistent,paired_publication}.rs`,
their supporting prefix/resident consumers and actor workbench/private-execution
composition in `exomonad/actor/src/resident_workbench.rs` and
`resident_actor/owned_workbench.rs`. Read runtime/actor contributor guidance.
The service-cells leaf owns shared compiler client/wire/output types; receive
its existing `CellProgram`/item issuers and item-bound successor descriptions.
Native-selection owns exact compiler/native binding policy. Do not duplicate
either policy in runtime publication.

Lifecycle supplies focused changes to original-source admission, source A/target B
retention, preview/runtime installs, generation gates and recoverable cleanup.
This leaf receives those changes in shared admission/publication files before
cutover; lifecycle must not independently rewrite these constructors. Review
joint source schemas and final custody/recovery fields with that owner.

At the audit base, actor `begin_private_execution` uses finite durable/ephemeral
admission; preparation constructs a view and all-value request. World preparation
has no executable consumer. `begin_cell_program`, item/declaration/turn settlement
and final publication explicitly refuse missing world capabilities. Existing
same-base checks and post-removal promotion witness are useful skeletons, not
an implemented positive path. Existing hosted successor passes use the finite
route and do not qualify this migration.

## Wave R1: replace the actual front door in place

Pair signatures with service-cells and compile real consumers immediately.
Migrate `begin_private_execution` and its durable/ephemeral owner wrappers in
place to capture the actual published world, semantic base, original source A
and target/private placement B coherently. Missing realization is not missing
logical state: use the same service's cold acquisition. Initial startup/host
preparation must establish a world through the existing production source and
compiler owner, not a fixture factory or a hidden finite bootstrap.

Capture A once from the issuing source transaction and retain it explicitly.
Preserve B's actual lexical lease separately. Validate original identity and
Open gate at real compiler admission, along with B's receiving authority;
never substitute `RuntimeCellAdmission::source_lease()`'s former target-only
meaning or the caller's current transaction for A. Reuse lifecycle's affine
source-bound preparation and Fresh/Bound/Consumed preview contracts.

Replace `snapshot_cell_program` and ordinary `prepare_checked_host_cell`
baseline assembly with authored source, protected templates/import facts and
the published-world client. Service planning issues the syntax/ordered plan;
the existing runtime admission reserves generations/slots/expected identities
once and binds the same plan/source/purpose/origin. Expression observation
names require the service's maintained collision witness. No include/injected
baseline vectors, all-value interface capture or compiler-view reconstruction
precede a supported hit.

Reuse existing compiler work/source transactions and `PreparedRuntimeCell` /
`finish_cell_program_admitted` settlement/sealing. Stow the machine before
compiler waits; keep prepared output, A/B leases and cleanup obligations with
the operation if the waiter disappears. No accepted-source uncertainty retries
through cold or any other path. Receive executable checked output from the
service's actual issuer rather than adapting a raw receipt into authority.

## Wave R2: item-specific private settlement

Adapt `begin_cell_program`, `RuntimeCheckedPrefix`, `admit_checked_item`,
declaration adoption and `CheckedTurnCompletion::settle` to the received world
output. Preserve their existing evaluator, binding/native owners, ordinal and
in-flight reservation checks. Do not add a World evaluator or alternate prefix.
The SAME `ExactCompiledItem` retains its successor description and reservation,
base/restoration frontier, output, producer, origin and expected item identities.

On actual item success, complete the existing world client with that description
and exactly that item's runtime delta. Bindings/observations carry actual
accepted binder identities, issued checked rows, defining scopes and captured
native delta; declarations/imports carry their accepted checked join and changed
lexical/source facts. Shadow/removal carries its expected old fact. Runtime
settlement does not infer compiler roles, native choices or dependencies from
names, successful execution or inventories. Service-cells owns authenticating
the returned compiler facts and the shared incremental product construction.

Compilation/typechecking of the complete cell still precedes authored effects.
Successful items advance private prefixes only; parked/failed items do not.
Later prepared output cannot expose later names. On any failed/cancelled cell,
publish no new names from that cell. Preserve prior separately committed cells,
completed external effect receipts and any real partial machine mutation.
Do not describe private completed prefixes as public commits.

An item completion must not retain the original operation directory, reservation
or ancestor world chain merely to preserve a successor. Transfer needed immutable
row/native/source custody to the existing owners before temporary preparation
is released. Keep predecessor and candidate ownership until their actual
publication/cleanup outcomes settle. Coordinate opaque release/refusal/unknown
outcomes with lifecycle; physical cleanup does not need renewed source authority.

## Wave R3: publish one coherent final pair

Extend existing `FinalExecutionIntent` to retain the final private logical
product alongside its exact original admitted base. Freeze final writes once;
do not rebuild them from broad binding history on storage retry. Assemble final
public remapping/removal using the SAME `PreparedBindingPromotion` after
`prepare_binding_publication_removals`, and its owner-issued post-state witness.

At `PersistentSession::attempt_public_manifest_admitted`, prepare all fallible
immutable backing/custody work before claim. Recheck the exact semantic base
under the existing exclusive publication owner. Preserve
`world_execution_base_is_current`'s source/owner/context association checks,
while replacing obsolete full-view preparation with maintained current facts
where necessary. Semantic drift refuses the successor without effect replay;
metadata-only restaging can reuse the same semantic intent. Do not import the
old finite semantic-rebase behavior into world publication.

Replace the current final-promotion missing-capability refusal with consumption
of service-issued compiler facts plus the actual prepared promotion witness.
Commit matching world/restoration association, binding promotion and visibility
epoch in the existing manifest callback. No second finalizer, service-side
actor-current slot or distributed commit. Unknown durable publication retains
the existing unresolved claim/fence and recovery ownership; lack of a reply
does not establish refusal or successful rollback.

No mandatory additional GHC call records Rust binding facts. The world client
assembles the already issued successor description with the actual settlement
delta. On the next compiler operation, the same ensure/acquisition flow carries
authenticated current roots/predecessor relationship and required changed blocks;
Haskell realizes against the exact predecessor or restores that same product
cold. Runtime retains values/grants; world restoration cannot revive them or
retired source authority. Persist/recover the exact logical product with the
existing notebook/source/native recovery owners, without replaying cells.

## Wave R4: delete the finite route in the same integrated tranche

The replacement is not ready for early Persona while both engines remain.
Remove obsolete production and positive-test entry points after porting their
real consumers in this tranche. Keep this disposition current in the diff:

| Current surface | Required replacement/deletion |
| --- | --- |
| `PrivateExecutionCompilerBasis::Finite`, parallel `begin_world_private_execution` split | One in-place private admission with a coherent world/source base; no finite enum alternative. |
| `RuntimeCellCompilerBasis::Finite`, `RuntimeFiniteCellInputs` | World reservation plus runtime authority/identity facts in the existing admission. Remove finite view/request/interface/catalog/specification baseline storage and getters. |
| `admit_cell_for_execution`, `admit_planned_cell_for_execution`, ordinary `admit_cell_with_plan` producer | Existing service plan/reservation admission, then one cell output consumer. Remove obsolete finite overloads and parser-reservation duplication. |
| `compile_view_for_execution`, `snapshot_cell_program`, actor `WorkbenchCompilationSpec` baseline fields | Authored recipe and actual publication-owner changes; retain only facts still required by a named runtime consumer. |
| `turn::prepare_cell_program_admitted_inner` finite command/offer builder and its finite compile wrappers | One service-backed preparation/END finish path. Preserve common output decoding/evaluator consumers instead of cloning them. |
| `ModuleCandidateOffer::select_cell_program(_with_catalog)` resident producer, finite `CheckedCellRequest` baseline adaptation | Remove resident caller/obsolete APIs; output certification consumes demanded issued service facts. Build-action portable fixture needs remain separately justified. |
| Ordinary `capture_value_interfaces`, `checked_value_context`, include/injected/full physical input export | Delete obsolete resident producers, not merely move them into publication/cold initializers. Keep an actual distinct runtime/native consumer only with its exact reason. |
| `admit_native_setup_cell_in`, `admit_host_carrier_cell_in`, startup/renderer preparation | Migrate compiler work through the same service. Keep distinct native/host purposes and proof consumers, never a finite engine escape. Coordinate renderer/source changes with lifecycle. |
| `check_cell`, standalone `run_turn`, inspection/expression/declaration helpers | Classify every production caller and migrate resident compilation/query work to the service; delete superseded resident overloads. Build-action or deliberately untrusted decoder inputs do not justify resident bypasses. |
| Positive tests constructing finite admissions, arbitrary contexts/worlds, trusted proofs or fake compiler results | Port contracts through real publication/source/compiler issuers, then delete those authority factories and obsolete assertions. Raw malformed wire fixtures remain local authenticator tests only. |

Audit actual callers of every removed API, including `cfg(test)`, facade/host,
renderer specialization, actor startup, fork/reload/recovery and checked mounts.
Do not delete exact runtime/native certificates because their old carrier also
contained compiler-only metadata. Coordinate low-level mixed-API narrowing with
service/native owners; preserve explicit build-action behavior and typed old-format
refusal, without an execution compatibility adapter. There is no fallback flag,
test-only finite engine or unmeasured cold route that rebuilds the old request.

## Production acceptance and delivery

The engine integrator joins S1/R1 early, then extends the same path before G2:

1. Real hosted `haskell` tool: captured nonempty Seed/import primary, bind from
   Seed, final world+binding commit, then later use of both Seed and the binding.
   Observe actual operation/world/worker/output identities. Repeat with
   declaration, explicit/empty import, expression, source-defined retained type
   and mixed multi-segment cells. A successful integer bind alone is insufficient.
2. Observe complete checking before first effect and private item ordinals.
   Fail/cancel a later item; no names from that cell enter public scope, prior
   committed cells remain usable and completed effects are reported once.
3. Evict predecessor and remove the first disposable output before a later use.
   Fork divergent worlds, replace worker and restore exact current logical facts
   without live-source recapture or effect replay. Preserve historical native
   child identity while unused old custody does not become a root.
4. Exercise source A revoked while target B stays live, and converse controls;
   owner/epoch/source changes, cancellation before/after acceptance, uncertain
   END, missing executable seal, parked execution and a disappearing waiter.
   Assert original/candidate custody and truthful primary/secondary failures.
5. Change semantic public state between final preparation and claim; refuse the
   stale successor with unchanged public world/binding pair. Separately exercise
   metadata-only restaging, commit refusal and unknown durable outcome. Recovery
   must select a coherent exact pair with the existing lifecycle owner.
6. Compile all affected production/cfgtest callers after finite APIs are removed.
   Review the deletion map and actual call graph; prove hosted startup, renderer,
   queries and recovery cannot enter the removed engine. Do not make raw source
   searches the only behavioral evidence of route replacement.

Use admitted pinned local-only commands. Start with affected runtime/toolchain/
actor/host production and test targets, discovering exact labels. Run focused
counted Rust controls and the owning actor notebook contract integration cohort
required by `exomonad/actor/AGENTS.md`. Tests/qualification own the joined Persona
and final bundle histories; send their concrete selectors and resource needs as
soon as the real consumer exists. Record source/artifact identity, selected and
executed counts, terminal status and evidence. Run owning formatting and
`git diff --check`; no compile-only or legacy pass establishes this plan's result.

Escalate an incompatible source/output ownership requirement with its smallest
real consumer/counterexample. Never resolve it by accepting caller-built
world/output pairs, weakening exact native choices, resetting preview tokens,
silently rebasing semantic state or restoring the finite compiler path.
