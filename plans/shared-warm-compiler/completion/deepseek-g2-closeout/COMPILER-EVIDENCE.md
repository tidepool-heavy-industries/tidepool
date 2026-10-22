# C: compiler-origin evidence and actionable refusals

This lane owns Haskell host classification and stage certification. Start at
code `29231ed323`. Read support-text.md and job-producer-observations.json under
`/srv/swarm/evidence/inanna/g2-peer-review-29231ed3-20261022/`.
Do not weaken nominal type, capability, stage or dependency identity checks.

## C1: bind host classification to the real admitted compiler context

Observed: retained Job authority is null in both Haskell wire products.
The type's original source SHA matches shipped Types.hs and its prepared layout
has the expected single strict lifted field. This does not identify every GHC
predicate, but the Rust decoder is not dropping an issued tag.

Confirmed source omission:
GhcPipeline.prepareTypedTarget passes pvExactScope's canonical admissions;
World requests prohibit pvExactScope. World selected proofs are constructed before
the callback actually runs, and authenticated imported TYPE entries also exist.
The callback is consulting an obsolete source of evidence.

Refactor that callback to receive the actual admitted host-origin context, using
existing CanonicalInterfaceAdmission/World type evidence and exact selected owners.
Bind it once before callback invocation. Source-origin authentication can be
needed for a TYPE-only import; it must not require granting SOURCE execution.
worldCanonicalInterfaces alone is insufficient if restricted to selected SOURCE
owners. Do not pass a broad inventory or a latest-module map.

Keep the cheap candidate scan for ordinary Int binds. For candidate host types,
retain an inspectable typed refusal when proof or representation is unavailable:
root owner; selected Text package owner; Finder outcome/location kind and owner;
proof origin/kind and source hash comparison; constructor/field/strictness mismatch.
Do not print source bodies, add always-on giant traces, or default all failures
to authority=None with no reason. Ordinary unsupported user types may still be
legitimate non-host values; host-carrier admission surfaces its precise refusal.

Controls: cold physical vs warm source-less Job; TYPE-only versus SOURCE-selected
proof; missing/wrong source proof; same-spelling wrong owner; wrong field/root;
fresh and prototype-reused carrier; actual parked command -> retained Job -> later
cell with no command replay. Inspect authority before Rust then execute through
the normal installer. Do not authenticate by spelling or ambient file bytes alone.

## C2: classify the actual stage-certificate failure

Checked Text's executable segment certifies and writes item-0; the following
declaration certifies against prior context. CertifiedProducts.hs:597 combines
several failure predicates. Introduce closed refusal constructors with bounded
offending owner/binder/expected-versus-observed evidence at their owning branches.
Reuse existing error transport and structured logging; do not parse rendered text.

Run host-text-first.hs once and retain the exact variant. A source-origin owner
recovered twice across stages is a lead. If confirmed, carry/reuse its owner-issued
certificate across the transition instead of allocating duplicate originals or
accepting equal payload as proof. If the actual failure is missing fresh source,
repair that source evidence path; do not merge the hypotheses.

The negative controls must preserve true duplicate definitions, changed
implementation under compatible interfaces, wrong stage/source and missing required
custody refusals. Add a mixed-stage history that continues into another cell and
publishes, not only a successful certificate constructor.

## Integration

C1 and C2 can be separate leaves with named hunks; C reviews their joined
GhcPipeline/CompilerRequest consumers. Coordinate with P for any actual native
row/manifest changes. Compile production worker and affected Haskell suites; run
translation/serialization fixtures if those boundaries change. No new compiler
architecture, reflection layer or permission shortcut.
