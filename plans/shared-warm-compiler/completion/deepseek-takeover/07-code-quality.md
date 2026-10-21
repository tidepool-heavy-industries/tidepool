# Code quality: complete the repair, do not build another framework

These are prioritized refactorings within the handoff. A/B/C/D own changes in
their existing areas; the independent reviewer checks their joined use. Do not
spawn a competing owner for the same files or turn this into an unbounded audit.
Each change must improve correctness/readability without widening authority or
adding unnecessary work. Component elegance alone is not acceptance.

## Required as part of the core repairs

### A: make a closed acquisition a real construction boundary

The current CompilerSourceSupport has optional maintained native facts, and its
getter expects that a closed support owns them. Yet enrichment paths can return
before all views have been repaired. Use private draft state for incomplete
assembly and a closed representation whose maintained facts are mandatory.
Make the finalizer the only constructor of usable support. The compiler should
force every returning path to supply closed facts, rather than adding another
runtime initialized flag or expect at consumption.

Keep the immutable evidence and the exact projection derived from it together.
Request construction should consume the closed owner and the actual advertised
selection, not accept independently matched metadata/custody maps. Delete old
partial-return/refresh APIs after migrating every production consumer. Preserve
structural sharing; do not deep-copy the inventory to obtain a simpler signature.

### A: stop accepting independently reconstructed identity tuples

checked_native_input_decision currently receives Original ArtifactId, canonical
ArtifactId and native owner separately. Several callers recover those fields
from different projections. The error has too little information to distinguish
missing state from a mismatched pair.

After tracing the actual Prelude refusal, prefer the existing owner-issued
paired input/reference type over parallel same-typed IDs wherever the caller
already holds that pair. If no existing type fits, introduce a **private** paired
value constructed only from the actual admitted input; do not expose a generic
constructor taking arbitrary IDs and call it proof. Preserve full binding and
issuer identity, not only artifact bytes. A historical edge carries its own
issued support; it must not be reinterpreted as an independently granted root.

Do not mechanically wrap every ID, or introduce a second authority registry.
The point is to remove caller matching work. Refactor only after establishing
which distinctions the real caller requires, and retain both valid-history and
wrong-pair/unissued-root controls. Reuse CertifiedNativeGroupSupport and related
existing sums where they already express the distinction.

### A/B: preserve typed failure information through the actual consumer

Use a closed diagnostic discriminator for the exact failed predicate, with small
expected/observed identity fields. Keep missing row, absent Original, wrong pair,
wrong payload and incompatible native witness distinguishable. Preserve the
original typed failure through actor/tool receipts and render at the boundary.
Do not classify by labels or parse a formatted error to decide recovery.

This is a bounded extension of the existing failure owner, not a new error
framework. Full traces remain opt-in; routine failures should be intelligible
without serializing every retained product. New cases should require an explicit
match rather than falling through a generic boolean recovery decision.

### B: encode the question in the query

has_observed_owner and has_home_owner must be different named contracts backed
by the same typed index. Count actual payload kinds exhaustively. Keep an
independent recomputation oracle for updates, particularly replacement and
overlapping snapshots. Never require callers to subtract packages themselves.

### C: express lifecycle phases as moves of existing owned values

Use the private settled/program/installer handoff sum from leaf C. Box the large
payload at issuance, consume it once, and retain the one existing cleanup guard.
Remove obsolete forwarding boxes or duplicate staging state only when the
measured new ownership cut makes them unnecessary. Do not add booleans describing
which optional fields happen to be initialized, a second scheduler, or a shared
mutable shadow of the actor's state.

### D: make fixtures select behavior, not manufacture authority

Keep fixed Haskell in adjacent declared files, compile the exact file through its
real consumer, and keep success and deliberate refusal on the same valid setup.
Prefer a small typed scenario parameter to unrelated boolean switches or string
rewrites when an actual shared fixture needs variants. Do not introduce a generic
fixture DSL merely for these few cases.

Whenever a stale fixture is retired, trace and delete production helpers used
only to forge its unsupported state. Preserve legitimate corruption/storage
tests as explicitly different consumers. A generated history's oracle must not
call the same update/finalization function as the implementation it verifies.

## Small cleanup allowed while already in the area

- Remove duplicate custody merges and refreshes rendered unnecessary by closure;
  retain cancellation checkpoints and exact refusal order where observable.
- Remove obsolete constructors, alternate routes, unused imports and incident
  comments. Comments should explain the current invariant or non-obvious reason.
- Name values by their authority/projection: public facade, World primary,
  retained custody, selected input, issued dependency and executable demand.
  Do not rename broad unrelated areas as part of this pass.
- Pass the issuer's complete result to consumers instead of reconstructing it
  from labels or broader maps. Keep source provenance and rendered evidence
  coupled where the model-facing consumer needs both.
- Update the map at the producer/consumer boundary actually changed. Eliminate
  obsolete status claims; preserve unresolved limits and exact test provenance.

## Retained next-tranche proposals, not G2 implementation scope

1. **Repeated SOURCE preparation:** qualification, graph construction and final
   capture revalidate/recapture overlapping facts. Investigate carrying an
   immutable validated graph to the later exact compatibility check, retaining
   its source selection and recovery obligations. Remove genuinely redundant
   work before adding a cache. Measure on the packaged Persona route first.
2. **SOURCE/boot distinction:** carried lexical adjacency loses a boot component
   while current-source projection explicitly refuses boot. Add the bounded
   regular-plus-SOURCE same-owner control before deciding to carry a richer edge
   or declare a uniformly unsupported operation. No authority bug is yet proved.
3. **Definer-first lookup:** retain ambiguous defining-association and ambiguous
   export-only negative controls alongside the actual two-reexport success.
   The unique-definer repair is source-reviewed; do not replace it with first-hit
   lookup or positional matching.
4. **Memo versus response completion:** clarify whether immutable compiler memo
   additions may commit before the outer query envelope is framed. That is not
   lexical/native publication. Do not build rollback machinery based on a false
   equality between those effects.
5. **World-delta no-op path:** existing equal-union/custody analysis supports a
   bounded possible shortcut, but it must retain exact owners and cancellation.
   The evidence is in compiler-world-delta-20261021/primary-custody-proof.md.
6. **Source-policy closure audit:** the two walkers' possible canonical/Original
   closure gap lacks a real producer counterexample. Retain the audit report;
   do not implement speculative graph widening.

## Reviewer completion rule

For each accepted refactoring, identify the now-impossible invalid state, the
deleted matching/repair step or bypass, and the real consumer that exercises the
replacement. Review one adjacent producer, one downstream/recovery consumer and
one test oracle. Record any intentionally retained alternate route and its
distinct contract. Passing helper tests plus a surviving public bypass is not a
completed migration. Stop at a coherent reviewed tranche and run its joined
history before adding more abstractions.
