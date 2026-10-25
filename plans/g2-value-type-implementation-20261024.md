# Coupled retained-value TYPE transport: implementation contract

Status: core implemented; joined review checkpoint. Candidate
`5e10df3bef27417b338f292720648b61c06c9925` on
`integration/g2-domain-runtime-20261023`; baseline code `2e60aa8101`.
Evidence root: `/srv/swarm/evidence/inanna/g2-value-type-20261024/`.
Constructor/cache controls pass 24/24. The final production core passes all
12 compiler corpus cohorts (1,052 programs) and the retained-Job actor regression.
Haskell lifecycle/selection controls pass 5/5 leaves across their recorded sources.
Joined actor checks expose constructor correlation, SOURCE-versus-TYPE exact-owner,
and HOME linked-code identity refusals. The guards remain intact pending review. Exact outcomes and
open questions are in
[the integrated review packet](g2-value-type-implementation-review-20261024.md).
These results do not qualify G2.

This document supersedes the open design questions in the retained-value TYPE
frontier and review-response documents. Argon's locked specification and the
reviewed clarifications are implemented as one boundary; old wire/API paths
are removed, not retained through adapters.

## Issued Rust product and shared worker API

The checked-value constructors issue one opaque complete product. In
SelectedCompilerInterfaces, checked_value_type_closure resolves the value leaf,
requires its exact selected binding in the issuing namespace, and walks only
authenticated Interface edges. Child bindings must match selected rows, owner
sets must match requirements exactly, and producer/interface seals must match.
Mark bindings visited before descent; preserve cycles. Use partition_point and
take_while on sorted proof edges, never a full-proof scan per visited node.

Canonical, ValueInterface and LexicalJoin rows are supported. Raw Original and
PackageCode payloads are forbidden in the demoted closure. The root is identified
by binding, not role. Build demoted support using fresh append_type_interfaces
custody and no native offers, choices, closures, or producing cache references.

P1 provides these crate-private accessors for P2; all fields remain private:
- CheckedValueArtifact::type_product() -> &Arc<CheckedValueTypeProduct>.
- CheckedValueTypeProduct::leaf_binding() -> ArtifactBinding.
- CheckedValueTypeProduct::leaf_entry() -> &Arc<ArtifactEntry>.
- CheckedValueTypeProduct::leaf_parts() -> Result<Vec<OriginalInputPart>, CompileError>.
- CheckedValueTypeProduct::dependencies() -> &imbl::OrdMap<ArtifactBinding, CheckedValueDependencyRow>.
- CheckedValueDependencyRow::entry() -> &Arc<ArtifactEntry>.
- CheckedValueDependencyRow::parts() -> Result<Vec<OriginalInputPart>, CompileError>.
Both product types live in declaration_context (a private child module is fine).
Rows retain exact physical custody; dependencies are cloneable immutable handles.
SelectedTypeInterfaces::from_checked_value returns Arc<SelectedTypeInterfaces>
infallibly by cloning the constructor-validated demoted projection.

CheckedValueTypeProduct holds physical rows/entries/bindings, never a ViewLease
or CompilerSourceSupport. CheckedValueArtifact owns its separate demoted support.
CheckedValueRowIssuer stores Mutex<Option<Arc<CheckedValueTypeProduct>>>.
Cache hits and produced reuse validate the expected complete binding/owner/entry
set and each physical row against the expected entry. Never overwrite a first
issuance or accept merely internally consistent cached facts. Preserve the
receiver-before-ancestor cache order and cancellation; prepare cannot reenter
a view cache. same_world_type_row compares the complete product and issuer facts.
Remove obsolete leaf-only CheckedValueArtifact serialization; migrate genuine
leaf inspection callers to the product's immutable entry accessor.

## Wire schema and ownership

One coupled row-fact encoder in compiler_world is shared by cold, successor and
operand writers. Canonical row fact:
[semantic_row, leaf_binding, certified_artifact, receipt, dependency_closure].
Bindings reuse existing two-digest encoding; leaf artifact equals certified
artifact. dependency_closure is ordered by full binding and contains
[dependency_binding, dependency_semantic_row]; unique module owners per closure.
The row CID commits to every association. Physical aliases do not affect identity.

- TPCWORLD v6: 11 fields; row section becomes [base, visible, dependency_rows].
- TPCWSUCC6: 7 fields [magic, restoration, blocks, primary_changes, visible_changes,
  dependency_changes, acquisition]. Update both successor writers.
- TPCWREST v6: existing 10 semantic fields. No physical images or paths added.
- TPCWCELLOPERAND2: 7 fields [magic, exposure, reference, blocks, offered_leaf,
  offered_dependencies, acquisition].
Dependency rows are sorted [binding, OfferedWorldRow]; edits are unique
[binding, OfferedWorldRow-or-Null]. All parts travel through existing
WorldColdRows::row, OriginalInputArenaTable and InputAcquisition.
TPCWINPUT1 remains unchanged and commits through the new bindings root.
Delete old versions from production readers; migrate tests and diagnostic readers.

Rust restoration owns a coupled persistent dependency row/count map. Counts
are per distinct live row-fact CID, never aliases. Net changed-CID transitions
drive dependency additions/removals. Unchanged product means no edit. Positive
to positive means no transport edit, even if references change. Last-reference
removal emits Null. Primary/lexical-only successors preserve the dependency map.

## Haskell admission, selection and retirement

WorldBindingRow owns leaf binding and dependency manifest. WorldInputFacts owns
one Map WorldArtifactBinding WorldDependencyEntry coupling admitted row and
positive count. Dependency-only value rows never enter visible bindings.
worldRowsByOwner applies only to base/visible, not historical dependency custody.

Cold: admit all rows through existing owned input transport; index dependencies
by binding; decode row facts; verify manifests' exact semantic/owner association,
transitive requirements and exact dependency census, then establish counts.
Successor: admit additions; form candidate base/visible/dependency rows and custody;
apply full alias change; validate inserted row facts against candidate dependencies;
derive net retired/inserted CID deltas and require exactly corresponding zero/
positive wire transitions before publishing. No mutation of held predecessor on
failure. No full-history scans on successor: use changed CIDs and retained counts.
Preserve FilePath custody counts and release only the last row referencing a part.

WorldTypeClosureEntry carries binding and semantic facts. A manifest-selected
resolver reads demanded World/operand dependencies with their corresponding
custody. Existing primary-only consumers stay primary-only. Check exact binding
and semantic agreement across operand, demanded World and applicable primary
contributions before GhcPipeline owner-map collapse/hydration. Equivalent physical
aliases reuse admitted tokens. Inactive historical versions stay passive.
Haskell checks row/root correlation and physical TYPE evidence, not ScopeProofV3.

Extend existing row/binding/index/file and capture-budget accounting, including
manifest entries and binding keys. No arbitrary new retained World limits.
Use checked arithmetic and budget/cancellation checkpoints before costly work.

## Acceptance and handoff

Run exact constructor/cache refusal controls, graph cycles and scoped mismatch,
cold-vs-successor differential histories with an independent full census oracle,
shared dependencies/last reference/net replacement, primary-only successors,
operand-local dependencies, wrong commitment/scope, physical alias equivalence,
and no namespace/runtime authority from dependency-only interfaces.
Alias test must prove same row CID; y=x alone is not that proof.

Run resident_captured_source_retains_the_parked_job_for_a_later_cell early on the
joined tree, then ordinary nominal/promotion/operand/version histories and the
existing actor notebook cohort, preview and capture histories. Run required
compiler/serialization fixture checks. Preserve exact source, executable hashes,
commands, nonzero counts, failure causes and cleanup evidence.

Purge old encoders/versions/backdoors and stale comments. Argon reviews the exact
pushed integrated candidate after scoped tests and joined histories execute.
G2 remains unqualified until its remaining exact-bundle gates pass. New authority
requirements or a need to widen another subsystem trigger a bounded planning
checkpoint; don't weaken identity to move the failure elsewhere.
