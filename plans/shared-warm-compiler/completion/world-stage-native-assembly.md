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
