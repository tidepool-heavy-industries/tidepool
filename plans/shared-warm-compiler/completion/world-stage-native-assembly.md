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
