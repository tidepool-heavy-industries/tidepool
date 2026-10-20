# Service-owned cell preparation and successors

Status: implementation plan, not implementation or execution evidence. Read
[the campaign plan](../plan.md) and [locked contracts](../contracts.md) first.
Source audit: `6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`, code `92fa518105`.
Record the actual published plan/code base at kickoff and inspect intervening
changes. This leaf supplies the compiler half of the sole resident cell path.

## Ownership and current gaps

The engine integrator receives this leaf. Owned production paths are
`tidepool/toolchain/src/compiler_world.rs`, `cell_plan.rs`, the world-output
branches of `checked_cell.rs`, `declaration_context.rs` and `certified_products`,
the existing world request schema/client in `tidepool/extract-cmd`, and the
corresponding `bridge/haskell` request, completion and GHC pipeline consumers.
Read their nested contributor guides before editing.

Native-selection owns resolution policy, exact native binding/carrying edges,
first-publication closure and current projection. Receive its reviewed API;
do not add another resolver in the world-output adapter. At kickoff assign named
functions in shared `declaration_context.rs`/`certified_products` files and receive
shared signature commits serially before parallel consumer edits. The service
supplies authenticated lexical/type/instance/scaffold obligations through the
existing demanded-product/source issuer; native selection resolves their exact
bindings and captures birth there. Neither reconstructs the other's closure.
Runtime-publication
owns `tidepool/runtime/src/session/{admission,turn,persistent,paired_publication}.rs`
and actor workbench callers. Agree shared signatures before editing their
consumers; send focused changes for that owner to receive. Lifecycle owns
original-source admission, generation retirement and physical process settlement.
Changing their wire or source-lifetime constructors requires a paired review.

Existing code is useful but not connected:

- `compiler_world::plan_cell`, `WorldCellOutputAdmission`,
  `CheckedWorldCellProgram` and `admit_world_cell_program` correlate actual
  plan/reservation/output/source/origin facts. The latter has no production
  caller at the audited code. It does not yet yield the evaluator's executable
  `CellProgram` and item-inseparable successor descriptions.
- `DecodedWorldSourceReceipt::demanded_product_inputs` uses
  `DemandNativeInputs`, certified receipt groups, projected semantic support
  and `CheckedValueImportAuthority::from_world_output`. It is unconsumed.
  `demand_primary_inputs` still takes a whole cold `snapshot_roles()`;
  `ProductInputFacts::from_demand` supplies empty lexical/scaffold fields.
  These are incomplete seams, not proof that inherited obligations are absent.
- Haskell `validateHeldWorldCell` excludes declarations and multiple inference
  segments. `compileHeldWorldCell` rejects checked-value home requirements.
  Runtime expression observation reservation is also incomplete.
- `retainHeldWorldSuccessor` is definition-only. Existing materialization
  refcount tests exercise raw source trees, not real successor compiler reuse.

The compiler universe is already persistent. Preserve the existing worker pool,
request scheduler, BEGIN/END/cancellation owner and optional retention policy.
`residentCompileOne` still constructs an attempt/HPT, and normal cycles perform
`depanalSourceModules`; the migration does not establish removal of that work.

## Wave S1: executable output through the existing issuer

Agree one received boundary with runtime-publication: authored `WorldCellRecipe`,
the existing service plan and runtime-issued reservation go in; existing
`CellProgram`/checked items, demanded execution evidence and item-bound private
successor descriptions come out. Extend current owners instead of introducing
parallel world evaluator/certificate types. The existing checked program retains
its exact reservation/base/restoration frontier, producer/profile, original A,
target B, operation origin, ordinal/generation and physical output certificate.

Wire `admit_world_cell_program` into the existing product issuance chain. Decode
the SAME captured common receipt, item manifests, typed segments and selected
row references once under its `PackageInterfaceValidation`/inventory operation.
Use existing source admission, `CertifiedSegmentOriginals`,
`PreparedCheckedItemOutput`, `ExactCheckedItem` and `ExactCompiledItem` issuers.
Do not manufacture `ExactCompilationRequest`, a finite baseline or a second
certifier to satisfy their former inputs. Change mixed input contracts where
needed so they consume the already authenticated world output.

Keep physical preparation separate from executable sealing. The runtime's
existing `PreparedRuntimeCell`/`finish_cell_program_admitted` ownership pattern
must consume the matching settled compiler transaction and cancellation seal.
Unconfirmed/retired END refuses execution and retains primary plus cleanup
evidence. A successful decoder or raw receipt cannot issue executable authority.

First feedback is compilation of the production library, affected test
configuration and real runtime/actor consumers against the paired signatures.
Then run one real imported `Seed` bind and later use through production owners.
This is a debugging control only; S2 and runtime deletion remain prerequisites
for Persona. Keep the old route unavailable to these controls.

## Wave S2: complete ordinary cell semantics

Reuse Main's whole-cell planning/inference and ordered item machinery. Extend
the held-world input adapter to cover declarations, import prologues, explicit
and empty imports, bindings, expression observations, and mixed cells with
multiple inference segments. Do not introduce per-item typechecks before the
existing whole-cell preparation. Runtime reserves original identities once;
the service plan supplies syntax/classification and a maintained world-name
collision witness for observation naming, without enumerating historical names.

Replace the `exactRequirements` refusal with exact demanded type closure for
checked rows, including source-defined nominal types. Preserve the original
row's issuer and dependency choices; equal interfaces or a runtime symbol do
not select a newer implementation. Remove the declaration/segment restrictions
only as their real output and ordered-prefix controls compile and execute.

Close all compiler-established obligations through the existing source/native
owners. Include selected-source claims, exact home imports (`import M ()`
included), transitive orphan/family visibility, primary lexical nodes, protected
scaffold imports/graph and checked thin-interface evidence. Preserve missing
lexical nodes and required owners as failed obligations. A source/type check
does not imply native permission; an inherited Unknown proof is not upgraded
without independently complete output evidence.

Use the actual demanded semantic support for product getters, certification and
instance closure. Replace cold full-role snapshots with keyed owner lookup;
do not hide broad scans behind a narrow returned view. Cold original bytes may
remain custody, but wholesale cold support/lexical vectors/native offers must not
become the semantic parent of a new product. Borrow an exact demanded inherited
offer from its actual issuer when required; do not discard historical provenance
merely because its owner also retains unrelated data. Native-selection's exact root, historical
edge and current-product distinctions remain authoritative. In particular,
unused historical A stays custody when B is selected, while a demanded issued
edge to A stays A and a genuine consuming conflict refuses.

Acceptance includes declarations with later uses, imports and empty imports,
an expression observation, a source-defined retained type, and a mixed cell
whose later item fails. Assert actual compiler-selected owners and exact output
identities, not just successful rendering or the absence of a diagnostic.

## Wave S3: logical successors and optional realization

Preparation captures a private successor description from its successful output
before the reusable compiler universe strips the target. It binds each actual
ordinal and predecessor prefix; later prepared items grant no visibility.
After runtime settlement, the existing world completion path combines that
description with only the same item's owner-issued delta:

- Bind/observation: actual accepted occurrence, full `SymbolIdentity`,
  `SessionVarId`, defining scope/generation and exact checked row reference,
  paired with its captured native delta.
- Declaration/import: the accepted checked declaration/join and its actual
  lexical/source/role changes, not a scan of completed modules.
- Shadow/removal: expected complete old binding fact and actual replacement
  or deletion. Final public remapping uses the runtime's same post-removal
  prepared promotion witness, not caller scope arithmetic.

The completion assembles already issued facts; successful evaluation does not
derive compiler roles or mint native edges. Reuse the existing Materializer,
MaterializedMap and current reachable block frontier. Apply actual changed
nodes, share unchanged roots/source/profile and retain exact immutable row
owners. Empty change keeps roots and stages no baseline blocks. A successor
must not own an ancestor world chain or disposable operation/reservation owner.

No additional GHC round trip is mandatory merely to record those Rust facts.
The next existing source-free ensure/acquisition flow carries the authenticated
logical product/predecessor relationship and required changed blocks/rows.
Haskell authenticates them against that exact predecessor, then realizes the
new product; matching source seals alone never select a predecessor. Missing
predecessor residency acquires the SAME current product cold in that flow.
Extend the existing versioned protocol; explicitly refuse obsolete meanings.
Do not add a registry, actor-current slot, second scheduler or replay loop.

Only the authenticated realization issuer calls `retainHeldWorldSuccessor`.
Share compatible immutable dependency tables/source backing; retain independent
mutable attempt cells and preserve producer/profile/captured-domain, actual
worker/NameCache and interpreter-epoch restrictions. A compiled target's lazy
HMI must not keep discarded output scratch alive: retain its promoted immutable
row/owner and decode it into the compatible realization after validation.

Reuse `WorldBackingLease`, `CompilerWorldCache`, admission-time accounting and
existing optional retention limits. These production owners already implement
retention/eviction and physical backing release; they are not new acceptance
evidence for successor sharing. Hold active worlds through actual END; clear
their private tables before release; count shared physical backing once and
keep failed removal quarantined/owned. Package EPS/finder stays epoch-owned.
Coordinate complete worker retirement with lifecycle rather than adding a
successor-specific cleanup mechanism.

Exact dependency reuse (arm B) is mandatory: a later ordinary cell must consume
the earlier compatible dependency completion through its real issuer, including
after predecessor retirement. Deeper checked-import/GRE environment reuse
(arm C) remains optional and evidence-driven. Do not confuse a remaining normal
depanal/import/typecheck floor with failed delivery of arm B, or call a persistent
process alone arm B acceptance.

The shared exact package universe, immutable keyed rows and existing bounded
worker/realization cache are mostly implemented foundations, not deferred
roadmap items. Verify that their actual cell consumers borrow keyed immutable
facts rather than snapshotting a full role census. Preserve package boot/profile
and source lifetime, TH/plugin execution eligibility, worker replacement,
interpreter-epoch compatibility, active-through-END holds and actual release.
Qualification owns controlled one-, two- and three-slot histories and byte/work
measurements using these existing owners; this leaf supplies the real identities
and counters. If those controls expose an independent pool/input defect, retain
its exact counterexample and assign a bounded repair leaf under engine/lifecycle
integration. It remains a campaign completion obligation, not an orphan follow-up
or authorization to redesign the scheduler/cache.

Cold acquisition traverses/authenticates current shared roots once, preserving
captured bytes, negative source resolution, exact historical edges and fixed
profile. It does not reread live disk, flatten custody into selected roots,
replay cells/effects or restore retired execution authority. Durable storage of
the exact current product is paired with runtime/lifecycle recovery before
claiming restart acceptance.

## Joined acceptance, checks and handoff

Run these through actual actor/runtime acquisition, not a handwritten world:

1. Captured nonempty Seed and protected imports on both cells: bind from Seed,
   atomically publish, then use that binding and Seed in a later cell. Assert
   selected graph, output/worker/world identities and result. Extend with the
   S2 syntax/type cohort before Persona.
2. Evict predecessor realization and remove the first disposable operation
   output; use successor successfully and observe genuine dependency reuse.
   Fork divergent worlds, drop one, then prove the other's lazy/type/native
   consumers still work. Test last release and failed-cleanup quarantine.
3. Replace worker; restore identical logical facts with a different realization.
   Demand previously unimported captured source and absence after live disk
   mutation; prove no effect replay and correct refusal for retired source work.
4. Pair unused historical alternatives with demanded exact historical children,
   equal interfaces/different bodies and genuine conflict controls through the
   received native issuer. Test malformed unused acquired rows too.
5. Cancel/refuse preparation and inject uncertain END; no executable seal or
   public advancement occurs. Later item failure preserves only private
   completed prefixes; the failed cell publishes no new names.

Use admitted pinned local-only repository commands, discover actual labels and
test selectors rather than inventing them, and retain exact code/artifact,
selected/executed counts, terminal status and logs. Compile Haskell production
worker and affected Tasty consumers, toolchain production/test targets and the
paired runtime/actor consumers early. Use counted Rust/Tasty runners; run
`just fixtures-check` when translation/serialization changes. Format through
the owning entry points and run `git diff --check`.

Handoff includes the consumed native/lifecycle contracts, real production
callers, removed request path, source/current roots versus historical custody,
and remaining acceptance. Qualification owns controlled timing. Supply actual
host-work and worker frontend/interface/finalized counters, changed block work,
logical/unique/pinned bytes and cold-versus-warm identity observations. Keep
depanal/import/frontend costs explicit; resident GHC is not a speedup result.

Escalate a concrete incompatible issuer/closure requirement or a need for a new
authority owner. Do not paper over it with empty proofs, full baseline capture,
a legacy fallback or another capability hierarchy.
