# World stage native assembly

The receiving contract is recorded in
`/srv/swarm/evidence/inanna/world-full-language-rust-receiver-contract-20261020.md`.
The receiver owns checked stage identities, ordinal coverage, captured FileRefs
and the closed executable/original item sum. Assembly consumes these owners; it
does not decode a second proof tree or recapture output files.

## Owner transitions

`prepare_world_cell_program` folds `CheckedWorldCellProgram::stages()` in source
order. Each stage supplies its own source receipt, module products and certified
products references. Decode those outputs once using the existing accounting
operation and actual captured bytes. Capture produced value interfaces once,
then select only each stage's own ordinal range.

Executable stages retain the existing transition:

1. `DecodedWorldSourceReceipt::issue_segment_originals` admits source facts and
   authenticates the stage's original native products.
2. `CertifiedSegmentOriginals::select_item` authenticates each actual prepared
   target and item certificate against the same original issuer.
3. The existing native target issuer and `IssuedOriginalOutput` retain exact
   dependency choices and private immutable custody.
4. Store each item's own prefix context and source lexical facts beside its
   output. Capture `PreparedCheckedItemOutput` against the common checked-cell
   owner after declaration certificates are available.

Declaration/prologue stages have no executable target or native entry. They
reuse the existing authored declaration mechanism:

1. Extend the existing private `NativeAuthoredDeclarationAdmission` owner with
   `from_world_stage(program, stage, source)`. It verifies the exact checked
   stage Arc, reserved Lib generation/actual owner and same generated source.
2. Forward that admission through `CertifiedSegmentOriginals::issue` to the
   existing `certify_products_with_validation` call. This issues the actual
   authored canonical origin through the existing certifier.
3. Select the actual authored original closure, retaining its exact native
   dependency graph, then issue `IssuedOriginalOutput` from that selection.
4. Factor the common post-source-authentication portion of
   `certify_same_offer_planned_declaration`. Validate the actual interface SHA,
   source digest, producer, planned inventory, exports, instances and family
   closure. World borrows captured source facts and actual aliases; finite
   admission retains its existing physical source checks.
5. Retain `PlannedCheckedDeclaration` with its actual certificate, source,
   receipt digest and complete `OriginalCompilerInputs`. The common checked-cell
   seal already handles declaration items without native executable outputs.

Proposed narrow helper:
`WorldProgramSegment::product_admission(&self) -> ExactProductAdmission<'_>`.
This borrows the existing source/binding/input facts; it grants no new authority.
No public trusted constructor, finite physical request, fake prepared program
or separate declaration evaluator is introduced.

## Prefix input decision

One `ProgramSourceSupportState` must move across the ordered stages. It cannot
restart at each stage or be replaced by the final context. Exact item snapshots
retain only their own prefix visibility, while native products retain the full
issued dependency closure, including mutual declaration support.

The current World input path always projects `program.primary_compiler_inputs`
and `ProductInputFacts::from_demand` supplies empty program source/lexical facts.
That must change before mixed stages can honestly work. Later stages need the
previous stage's actual selected canonical roles, issued private native offers,
produced value interfaces and inherited source/instance facts.

The existing `ExactRequestInputs` owner is semantic and separate from the
physical `ExactCompilationRequest`; its private context advance already carries
`OriginalCompilerInputs`. Its current advance also scans baseline metadata and
groups. The receiving engine and assembly owner must choose a shared semantic
continuation that consumes keyed issued support deltas, rather than import that
broad finite work or build a second compiler-input registry. The demanded stage
owner should own its context Arc so a moved continuation does not borrow itself.

## Ownership and acceptance

The receiver owns compiler_world proof/admission types, World source binding and
identity, receipt decoder, and checked_cell closed-variant consumers. Assembly
owns artifacts plus the coordinated declaration issuer/helper slices. Engine
owns runtime declaration projection, actual settlement and logical successor
updates. Haskell owns the actual ordered producer and retained compiler prefix.

First acceptance is compilation of affected production and cfg(test) consumers,
then actual emitted declaration/prologue/executable stage controls with useful
crossed-owner/interface/source/ordinal refusals. The joined mixed-language actor
history and subsequent value use remain the receiving engine's gate. Linking,
receiver DATA controls and compiler native evidence do not establish runtime
execution authority.

## Common declaration certifier checkpoint

The first source tranche extracts shared validation from the existing finite
declaration certifier. Its finite wrapper preserves the same source preconditions
and physical source reread. The shared private helper performs the existing
producer, selected original, canonical declaration origin, inventory, lexical,
instance and family closure validation. It is not yet a World stage consumer.

Build `dfa9b53f-9c0f-41f8-bd1b-6d5679ea564c` compiled production and cfg(test)
consumers and executed three existing controls: two passed, one failed, exit 1.
The generated issue/retention history and inventory refusal passed. The
multiversion-custody consumer failed at its pre-certifier native-selection
refusal assertion at `planned_original_history_tests.rs:590`.

An exact inverse of this extraction against `0e3fde7cd2` reproduces the same
failure: build `9b2b22fb-01a1-405f-8e30-1cf1138b82d0`, executed 1, failed 1,
exit 1. The extraction was reapplied afterward and `git diff --check` passes.
No assertion or native qualification rule was changed. The exact returned
native result and cause still need classification; the failed expected variant
alone does not establish a permission regression.

Evidence roots are
`/srv/swarm/evidence/inanna/world-stage-declaration-factor-20261020` and
`/srv/swarm/evidence/inanna/world-stage-before-declaration-factor-20261020`.
The inverse patch is retained at the first path plus `.patch`. The extracted
Rust executable SHA-256 is
`3fa7ddd5b304479c18867c593f5fb3265edab0f08370ea1acf46aa290d653650`;
the pre-extraction executable is
`743c75bb11772e70b3698dd073225c8c3ebdffc33f5ff440a704415d451ac21a`.

## Native row owner checkpoint

The same `RequestNativeAvailability` row owner now stores an
`Arc<imbl::OrdMap<ArtifactId, Arc<NativeTransportAvailability>>>`. The outer Arc
preserves existing acquisition identity; the persistent map shares untouched
structural nodes. Merge rules, contribution handles, qualification and typed
refusals remain unchanged. The completion handoff borrows this issued owner
through `issued_row` rather than requiring a raw map representation. Independent
reference transport tests retain their BTreeMap model at observation boundaries.

The genuine issued-row insertion control keeps an immutable pre-update snapshot
and constructs the expected post-update fact map independently. Across 1/32/128
unrelated rows, one added native row changes exactly one maintained fact and
copies 3/8/8 baseline row references through structural paths. Every unchanged
row retains its exact Arc and contribution handles. This does not yet establish
the complete staged continuation's cost: the existing native offer slices and
private input vectors can still copy or scan broader pointer inventories.

Final build `77af94ea-7e6d-4605-bd37-f4a6590f358b` compiled production/cfg(test)
consumers and executed six controls, **6 passed, exit 0**: changed-row snapshot
isolation, unchanged root reuse, exact historical dependency intersection,
missing/conflicting issued-row refusal, exact original identity among equal
owners, and actual producing-cohort completion. Evidence:
`/srv/swarm/evidence/inanna/world-stage-native-persistent-rows-final-20261020`.
Earlier compilation failures exposed raw-map assumptions in the completion
handoff and test observation types; they are retained under the corresponding
`world-stage-native-persistent-rows*` evidence roots. No broad map conversion was
added to a production handoff to satisfy those errors.

## Checked stage issuer checkpoint

The accepted receiver chain through `3e91eec6ce` is received locally. The first
assembly tranche derives `NativeAuthoredDeclarationAdmission` inside World
source issuance from that source's exact checked declaration/prologue stage.
It checks the same stage Arc, reservation kind/generation, actual Lib owner,
admitted generated source and ready source evidence. The existing segment
certifier consumes that admission; finite executable consumers still pass no
authored admission. Native original entry evidence is never invented.

Named module-product/certificate outputs and nested payload roots now use their
actual stage namespace. H confirmed that nested receipt paths are relative to
the stage directory, while executable item files remain under `item-N` at the
preparation root. `WorldProgramSegment::product_admission` borrows the existing
source/binding/input facts and is reused by its original-execution consumer.

The first four receiver/source/declaration history controls passed, build
`5baab79a-8f00-48be-a34c-610ed7b32a66`. After the final explicit generated-source
owner check, production/cfg(test) consumers compiled and the stage-ownership and
generated declaration issue/retention history controls passed again: build
`a3288c65-403c-49a6-a3a3-2aaf63915dc5`, executed 2, passed 2, exit 0. Rust SHA-256
`779809ac265d71287fd4fbe0a076ccb0ce7ba10cd4d06987e8f7d9d1313bc9e8`.
Evidence:
`/srv/swarm/evidence/inanna/world-stage-authored-issuer-final-20261020`.

This is receiver/issuer compilation and existing component coverage. Ordered
multi-stage assembly, the keyed source/native prefix continuation and actual
mixed-language output remain unfinished. The earlier actual native and cold
restore gates apply to `ad99ed7700`; they do not qualify the new PREP4 schema.

## Captured file follow-up

Confirmed source facts: `compiler_world.rs::capture_world_file` retains each
named file as a `WorldOutputFile` and invokes
`recovery_artifacts.rs::PackageInterfaceValidation::record_captured_bytes`.
That method records digest/size observations only when its observation scope is
enabled; it does not provide an immutable byte source for later loaders.
`prepare_world_cell_program` creates a new validation owner with the same
inventory accounting operation.

Unconfirmed implication: native finalization/sidecar consumers may reread some
already-captured paths. Trace those exact consumers before calling this redundant
or deleting an intentional independent authentication observation. If they must
consume the same captured output, lend the actual existing output owner through
the product loader; do not add a second output cache or duplicate payload tree.


## Authored closure custody and checked scope projection

The authored stage now retains the same `ArtifactView` returned by
`issue_segment_native_availability`. `WorldProgramSegment::planned_declaration`
passes that actual view through `SelectedOriginalClosure`, `IssuedOriginalOutput`
and the common declaration certifier. The World wrapper checks the same stage
Arc, admitted captured source, reserved Lib generation, actual canonical
interface SHA and inventory. It does not reread the captured source or issue a
synthetic executable target. This helper is compiled; the ordered stage fold
and a genuine mixed-output consumer are still pending.

`NativeOfferCustody` derives `required_scopes` during its existing checked-node
acquisition traversal. Artifact carrier scopes and group issuer scopes are
recorded independently. The exact proof references and their direct ancestors
remain with the original issuer; this set is only a borrowed projection.

At parent `b1eef86c4b` plus this source delta, the focused command
`swarm-build just test-lib tidepool-toolchain --exact declaration_context::tests::native_availability_delta_preserves_snapshot_and_untouched_row_owners --exact certified_products::tests::physical_native_issuance_survives_item_type_projection_without_future_demand --expected-count 2 --output-dir /srv/swarm/evidence/inanna/world-stage-native-scopes-final-20261020 --retain-artifacts`
compiled the production and cfg consumers and passed 2/2, exit 0, build
`e8730080-8439-4a2a-8f1d-cb5dcb8a65b8`. The executed Rust artifact SHA was
`2cc6356cf13ef0d99d364e0baf9ae15bc2642d6d09757a704cd2d1d6e4e51429`.
The controls include 1/32/128 persistent native row insertion, isolated prior
snapshots, and an actual issued graph with different group/carrier scopes.
The first compile attempt `7106bf09` caught an inventory JSON borrow type
mismatch; the corrected wrapper borrows the existing string's bytes.

Current source-support work is separate from runtime lexical publication. A
stage Lib declaration certificate describes its actual authored exports and
full compiled closure; it is not a cumulative `AcceptedJoin` facade. The
runtime join owner must supply that authority. A separate open native boundary
is how a new explicit Original role affects prior optional Full qualification;
no full retained-native census or unchecked promotion is accepted as its repair.


## Keyed program-support checkpoint

The shared custody append now accepts exact admitted bindings and opaque native
offers. It follows their actual issuing scope references and direct ancestors,
merges same-payload custody through the existing owner, and retains only the
native offer's actual artifact carriers as passive custody. It does not add
Group roots. TYPE insertion delegates to this same mechanism.

`CompilerSourceSupport::append_program_fragment` updates only incoming admitted
roles, exact canonical/Original binding pairs, native availability rows and
recorded input choices. It returns the changed owner keys. An actual existing
InterfaceOnly-to-Original promotion explicitly invokes the established native
qualifier once. Fresh-owner additions and canonical-only insertion do not call
that qualifier. Prior exact historical contribution graphs remain with their
issuers. The native offer reference array still copies its retained handles;
that residual is separate from selected-role and metadata work. Existing live
view registry cleanup is retained.

The same `RetainedArtifactMetadata` now remembers its greatest-ID non-native
representatives during the existing cold metadata traversal. That category
includes canonical interfaces, value interfaces and lexical joins. Keyed
updates preserve the existing unique-native overlay and ambiguity fallback
without scanning retained payloads. These catalog representatives never issue
compiler roles or native permission.

`RequestCompilerInputs::with_declaration_projection` accepts the actual
`AcceptedJoin` and its associated issued projection context. It checks the
facade's exact canonical binding, consumes the join's existing compiler support,
and returns coherent inputs/context/changed owners plus the promotion qualifier
count. The resulting context retains the supplied lexical, instance and
template facts. Only changed Original roles get direct advertised native roots;
new private dependency custody does not become a direct root. The existing
advertised-owner map now shares persistent structure. The engine owns runtime
receipt/context correlation, materialization and atomic adoption.

At parent `5a36bf7391` plus this delta, command
`swarm-build just test-lib tidepool-toolchain --exact artifact_inventory::compiler_projection::tests::program_original_promotion_qualifies_once_and_keeps_issued_dependencies --exact artifact_inventory::compiler_projection::tests::program_fragment_metadata_matches_cold_multiversion_representatives_and_releases --exact artifact_inventory::compiler_projection::tests::ordinary_native_fact_successors_encode_only_changed_facts --expected-count 3 --output-dir /srv/swarm/evidence/inanna/world-stage-program-inputs-compile-20261020 --retain-artifacts`
compiled production and cfg consumers and passed 3/3, exit 0, build
`539f6e9d-2015-4a0e-ba89-c8caa1981c1a`. Executed Rust SHA:
`051492dcac960c7f9017651208f0ffa6855a9ba1d89dfb368351462ef9380638`.
Controls compare a retained prior snapshot and the general merge oracle,
check exact retained issuing offers/dependency carriers, compare metadata to
cold recomputation in both native insertion orders, and verify that the prior
snapshot does not retain the new payload after the updated metadata is dropped.
The existing TYPE fixed-work control also passed in the preceding 4/4 run
`ce5d8362-f1eb-46a0-9e52-31311f26a2dd`.

The ordered stage fold and a genuine mixed declaration-to-later-use consumer
remain pending. The new AcceptedJoin input hook is compiled but has not yet
been exercised through the runtime's actual declaration adoption consumer.
The selected input-decision owner is receiving a separate live-acquisition
repair from its owner; program insertion must use that owner's merge helper
when it lands, preserving those same live receipts as well as recorded facts.

The inherited planned-original negative case was separately observed with its
oracle unchanged. `planned_original_history_tests.rs` emits independent groups
7 and 11 in one selected Original, then deliberately rebounds it without the
issued Full closure. The current issuer first refuses group 11 with
`NativeGroupUnavailable`/`InputNoCompatibleCandidate`; the test hardcodes group
7. Matched v26 run `94452c0f-8785-4ecb-9f10-df9217ab28d9` executed that case and
failed its assertion, Rust SHA
`5786a750a20418095e77d7baf2900fb373dd97901d552e05bda337a3d7b35909`.
The exact diagnostic is retained under
`/srv/swarm/evidence/inanna/world-stage-matched-promotion-diagnosis-20261020`.
Independent oracle review is separate; no expected refusal or product
permission was weakened here.


The separately approved refusal oracle repair derives the exact required native
group set from the current certified Original, asserts the fixture's groups
are 7 and 11, and establishes that every actually issued binding for each is
missing from the rebound view. It requires the typed refusal to name the exact
current artifact and a member of that deliberately missing required set. The
positive multiversion/selection and genuine ambiguity controls remain intact.
This removes only the unsupported first-refusal ordering requirement.

At parent `49e4fbb79e` plus this test delta, the exact rebound case and unchanged
generated issue/retention property passed 2/2, exit 0, build
`c63abe4d-156d-4aba-88f8-d2695cd0388b`. Executed Rust SHA:
`bd6f938783feb7eb438d91f288a26533a68765c2a3581701378768fa90a10eca`.
Command: `swarm-build just test-lib tidepool-toolchain --exact certified_products::tests::planned_original_history_tests::selected_planned_original_survives_multiversion_custody_at_consumer_boundary --exact certified_products::tests::planned_original_history_tests::selected_planned_original_matches_generated_issue_and_retention_history --expected-count 2 --output-dir /srv/swarm/evidence/inanna/world-stage-original-refusal-oracle-20261020 --retain-artifacts`.


## Supplied source-role projection

The authenticated source selection now resolves its supplied artifact IDs via
keyed observations of the same acquired payload owner. Generated-scaffold
omission still requires the actual admitted generated owner and its absence
from the existing acquired-owner index. It does not infer a namespace or native
permission from custody. The shared role builder keeps all non-Original
interface kinds, checks each selected descriptor's exact owner, and retains the
same Original owner/canonical/issued-row checks and final `from_issued_entries`
admission. Other metadata-based consumers retain their existing entry points.

At parent `fedd0ae226` plus this delta, the source projector and unchanged native
issuance control passed 2/2, exit 0, build
`93bbc75e-f355-47e4-bc09-2be2d72a50c4`. Rust SHA:
`b9ac32afbb1f4509e0fbb0947b7a7cd7ca1e95944d023e109211ec661822616f`.
The projector control supplies actual canonical, ValueInterface and LexicalJoin
rows to a cold view with 1/32/128 unrelated rows. Each run observes exactly the
three supplied roles with zero view queries, graph visits or metadata handle
copies, then compares against the independent metadata entry point. It also
refuses a crossed selected owner and missing required payload.
Command: `swarm-build just test-lib tidepool-toolchain --exact certified_products::tests::source_support_projection_uses_only_issued_keys_and_keeps_all_interface_kinds --exact certified_products::tests::physical_native_issuance_survives_item_type_projection_without_future_demand --expected-count 2 --output-dir /srv/swarm/evidence/inanna/world-stage-keyed-source-projection-20261020 --retain-artifacts`.


## Ordered stage consumer checkpoint

The World consumer folds the checked stage table in order. It decodes each
stage's named receipt and products once, carries one move-only prefix containing
its context, inputs, program source state and same private Original input, and
retains each source admission until the common checked-cell adapter validates
the complete declaration map. Original stages use the same issued physical
view, Original output and common declaration certifier; executable stages reuse
the existing target/native issuer. Each item retains its own resulting prefix.
No finite request or executable placeholder is created for an Original.

The World prefix no longer calls the finite support continuation's broad
metadata rebuild. It validates the actual supplied role rows, appends their
keyed support, and updates the same persistent lexical owner. The common source
edge filter is shared with existing finite callers. Program input decisions
merge through their owner's selected-acquisition helper, retaining live custody
separately from recorded facts. Native offer reference-array copying, the
existing view-registry cleanup, declaration certificate closure work, and
per-item lexical slice observations remain explicit costs.

At parent `7d756fd749` plus this delta, cfg compilation and three controls passed
3/3, exit 0, build `2c817573-b5fe-4954-8e3a-1e90e2e3a78c`. Rust SHA:
`b9ff8fe576a0839a80eb4a0873b2eaaaa2c1b412469a6e124d24e21798a594d7`.
The lexical append control compares cold recomputation and retained old facts
at 1/32/128 rows, checks equal-owner sharing, and refuses changed adjacency.
The keyed source closure compares the general policy oracle with authentic
session implementation kinds and observes one adjacency lookup despite
unrelated baseline growth. The existing native snapshot isolation control
remains unchanged. Command: `swarm-build just test-lib tidepool-toolchain --exact exact_context::tests::lexical_append_shares_unchanged_facts_and_refuses_changed_adjacency --exact declaration_join::tests::keyed_source_closure_matches_general_policy_without_baseline_walk --exact declaration_context::tests::native_availability_delta_preserves_snapshot_and_untouched_row_owners --expected-count 3 --jobs 1 --timeout 300 --output-dir /srv/swarm/evidence/inanna/world-stage-ordered-fold-compile-fixed-20261020 --retain-artifacts`.

A local formatting procedure caused the first ordered-fold compile to fail
(`c64097d9`): selecting individual formatting edits by text alignment paired
repeated blocks incorrectly during the large owned function's reindent. The
complete formatted function was retained and correct. Replacing only that
wholly owned function from the retained copy repaired the source; the following
compile and controls above passed. For an owned function with large repeated
blocks, replace the complete function from the formatter output and review its
boundaries rather than selecting diff alignment edits. This is a local tooling
finding, not a compiler-product refusal.

Mixed declaration-to-later-use and runtime declaration adoption remain
unqualified at this checkpoint. The matched Haskell mixed producer must be
received before those controls. Within-cell stages also do not establish the
separate declaration-cell-to-next-cell runtime and cold-restoration workflow.

The existing authentic World bind control also passed 1/1, exit 0, through
the ordered fold: build `acb47003-efd8-463c-8cd1-2929556e37f0`, same executed
Rust SHA as the three controls above. It exercises actual prepared native
evidence plus the matching compiler END and post-END cancellation refusal.
Command: `swarm-build just test-native //tidepool/toolchain:toolchain_world_native_seal_test --output-dir /srv/swarm/evidence/inanna/world-stage-ordered-fold-native-20261020 --retain-artifacts`.


## Stage decoder and lexical owner follow-up

The source receipt issuer now decodes its own two authenticated named stage
outputs from the already captured bytes. Callers cannot supply independently
parsed module products or a certificate from another stage. The old borrowed
package-output getter and corresponding artifacts-layer parsing were removed;
no additional read, raw tree or certifier was added. Actual missing/swapped/late
raw-output replay controls belong to the checked receiver and remain pending.

The existing program-source lexical field now uses ExactLexicalSnapshot.
Finite replacement retains replacement semantics; World updates append only
the actual incoming rows. Existing borrowed slice consumers use its explicit
lazy observation. At 1/32/128 prior rows the append control observes one supplied
row and one fresh immutable fact; overlapping input observes two supplied rows
and one fresh fact. Equal and conflicting adjacency, unchanged snapshot sharing
and independent recomputation remain checked. The unchanged finite import
history oracle passed with the two lexical controls: 3/3, exit 0, build
`54e90d13-61fc-4d7e-a87d-07234b364546`, executed Rust SHA
`42eb1ee2b67808ce3e0b82ac2f3cf2dbead77d0e0cd2bcd8b1d7ef92fac9909a`.
Evidence: `/srv/swarm/evidence/inanna/world-stage-lexical-field-controls-exact-20261020`.
The first invocation named the history case incorrectly; counted discovery
refused that selection before execution. It is not included in the passing
count. The correct path is
`declaration_context::tests::program_source_support_history::bounded_artifact_and_original_import_histories_match_set_oracle`.

The actual mixed consumer fixture is being qualified separately. It first
exposed a missing declared renderer source closure, then a missing canonical
whole-cell template marker. Both are confirmed fixture setup defects, repaired
by capturing the real declared facade library bodies and using the maintained
canonical template shape. Build `10383170-3d3e-4195-8197-e504402ee2c2` compiled
and reached a producer policy refusal: WorldDownsweepGraph marks captured
`Tidepool/Aeson/Scientific.hs` as generating code while the admitted flags do
not. Quasiquotes, Template Haskell, static pointers and external preprocessing
were all false. The cause is with the Haskell owner; native mixed admission has
not been reached and no policy is weakened here. Exact stderr is retained at
`/srv/swarm/evidence/inanna/world-stage-mixed-canonical-template-20261020/171cda3b8d0b174296a588fc09cb6c7cce74ba26a7b587f9b4256a11a719bfa2/artifacts/compiler-transactions/.tmpXGKEj4/compiler.stderr`.
The original diagnostic files remain present even though the transaction's
product-file roster excludes its own stderr/report files. An initial hypothesis
of diagnostic deletion was disproved by direct observation.

## Genuine Original and mixed consumer controls

The two new named targets use the same admitted compiler, captured source owner,
World recipe and reservation, native issuers, END evidence and cancellation
control as the existing successful bind target. The declaration-only source
contains an ADT, a single-method class and its instance. The mixed source binds
a value, declares an ADT and class instance whose body uses that value, then
binds the ADT and calls the declared method. The mixed reservation has three
stages and four execution items. Its assertions distinguish the early item
context from the later context containing the actual Lib owner. These are
compiler/native component controls; they do not execute a runtime actor.

The target generator owns their compiler resources and fixture registration.
The fixture captures the actual declared facade library sources, including the
renderer, through the existing captured-source constructor. Its template uses
the canonical effect-row marker and the shipped FlexibleContexts and
UndecidableInstances extensions. It does not suppress generated renderer work
or enable Template Haskell.

The earlier Scientific.hs backend refusal was a receiving-base defect: this
checkout lacked the already reviewed Haskell commit
`8c9434e4630e47ebcbede4aff5d8fca9cd3ae167`. Receiving it as `b477cf51e3`
restored the complete quote-free graph backend lowering. No new compiler policy
change was needed. The following run exposed an auto-display family constraint
requiring UndecidableInstances; the canonical fixture now supplies that shipped
extension. The Haskell owner is separately correcting optional auto-display
generation for minimal recipes that lack it.

The mixed consumer is still red before Rust preparation admission. Run
`ced2beaa-e60e-4858-b2e0-ab0d8aa18646`, exit 1, reached
`a prior original was replaced by another finalized capture` in the Haskell
producer. The actual stage products and interfaces remain under
`/srv/swarm/evidence/inanna/world-stage-mixed-renderer-types-20261020/171cda3b8d0b174296a588fc09cb6c7cce74ba26a7b587f9b4256a11a719bfa2/artifacts/compiler-transactions/.tmp2524or`.
The independent observation at
`/srv/swarm/evidence/inanna/world-stage-prior-finalization-observation-20261020.json`
establishes that seven retained finalized interface bodies have identical
SHA-256 hashes across the two stages, while the actual native module-product
sets differ. It does not establish equality of their entire finalized owners.
The possible conflation of canonical TYPE admissions, native Originals and
relocated physical interface aliases is under the compiler-products owner's
review. Native identity checks remain unchanged.

The independent declaration-only target also remains red before Rust
preparation admission. Run `c08766f8-3b9e-4244-a298-15ce8f0a3e03`, exit 1,
compiled and executed one selected case using Rust SHA
`d55a3d3ef6eb0bc15b35738fa73de294926adda59e95f37c76a662d0d32ca561`.
It refused native publication for the actual exported binder
`main / Tidepool.Session.Lib.G21 / value / C:OriginalStageReader`, which lacked
a certified native owner. The stderr is retained at
`/srv/swarm/evidence/inanna/world-original-native-seal-20261020/2594a133c5967eaf4c9d9fd193aba077f053226ea0f9347a0642e7087afed50e/artifacts/compiler-transactions/.tmpeQtTd7/compiler.stderr`.
The single-method dictionary constructor may be structural or erased; that is a
hypothesis for the owning GHC root classifier. The fixture keeps the class and
requires an authentic successful declaration packet. No spelling-based skip,
dummy native group or expected-error acceptance was added.

Actual missing, crossed and later-stage bundle replay controls are supplied by
the checked receiver owner and will be invoked after matching compiler END.
They require a genuine completed multi-stage packet, so neither red producer
run qualifies those controls or the Original native certifier.

## Paired stage demand caller follow-up

The PREP5/worker27 continuation is based on compiled engine source
`37787bf18f4e35e348bb1e5c3c87e74f5228ef25`. The original native branch remains
preserved at `96cfa1381b`; the paired continuation branch received only that
fixture parcel as `4c2a3d91a3`. An attempted ancestry merge encountered duplicate
cherry-picked history conflicts and was aborted before any resolution. No
unrelated compiler, runtime or protocol source was rewritten.

Native item selection now captures external selected value owners from the
same stage's demanded rows. Prior same-cell value interfaces remain selected
by the existing produced-output ordinal prefix. Source-edge exclusion likewise
uses the same stage rows plus the exact sealed plan operand as independent
compiler availability, without changing runtime roots or using the program-wide
union. This does not generalize compiler availability to every visible BaseWorld
binding; the compiler-only unused BaseWorld-helper case remains an open boundary.

The mixed fixture additionally requires no preexisting runtime roots and the
actual first binding as its sole PriorStage runtime root. It checks the exact
three-stage order, prior-origin coordinates and absence of fabricated checked
rows for those same-cell inputs. The receiver's assertion-only raw-output replay
helper runs after matching END, outside the active compiler transaction, once
on the successful mixed preparation. It changes only cloned packet data and
requires one positive replay and twelve typed bundle-role refusals.

This caller follow-up is source-only until the affected cfg target and genuine
mixed native gate execute. The independent actual Some input gate exposed the
projector's dependence on a lazy cold metadata cache after a coherent TYPE
delta. The engine owns the separate RequestCompilerInputs boundary repair; no
metadata fallback or pointer-check removal is included in this caller parcel.
