# Physical stage versus carried compiler prefix

P owns phase types and producer/caller migration. Start from the actual three
cohort issuers: ordinary product issuance, ArtifactInventory::bind_segment, and
authored segment issuance. Preserve their valid source-specific checks.

Extend/rename PhysicalValueCaptures into private `CapturedCompilerStage`, owning
operation, actual captured facts, demanded-row issuer and finalized whole-stage
output. It alone exposes PhysicalCohortInputs, without an optional-capture test.
Keep IssuedOriginalOutput for carried compiler output/prefix publication; selected
item prefixes do not gain physical capture authority. Empty/Partial are valid
completed compiler outputs, not promises of Full executable availability.

BoundArtifactSelection holds the stage plus the existing ordered prefix cursor.
Original-only stages hold the stage without an executable cursor. Target admission
takes its actual stage/item owner, not separately assembled cohort and selection.
Do not eagerly link availability; carry B's references for demanded targets.

Delete crate-visible OriginalCompilerInputs::from_selection and the raw
IssuedOriginalOutput::issue(selection, view) constructor. Keep normalization private
to actual issuance and valid input-only transfer through from_issued_support.
Remove physical: Option from transferable OriginalCompilerInputs. Converge the
three producers on the stage owner; do not create a second issuer.

CompiledArtifacts carries its issued output owner and derives selection access.
ExactDeclarationContext and authored declaration joins consume that owner rather
than recreating it from separate artifact/selection fields. Current compiler roles
come from the closed support projection, not ArtifactView's historical custody
namespace. Keep artifact_ids_from_entries as an internal projection detail.

Migrate tests by purpose: custody-only data stays data; input transfers borrow an
existing support; outputs pass the real stage issuer. Retain malformed-wire tests,
but remove positive fixtures that manufacture executable authority. Preserve the
passing Empty/Partial oracle and exact historical dependency controls. Runtime
membership, availability, stage identity and source permission still need typed
runtime refusal; do not claim Rust can statically prove arbitrary supplied facts.

Coordinate shared declaration_context.rs and compiler_projection.rs hunks with S/B
using isolated branches and early shared-contract commits. Compile production and
cfg(test) callers before claiming the phase migration complete.
