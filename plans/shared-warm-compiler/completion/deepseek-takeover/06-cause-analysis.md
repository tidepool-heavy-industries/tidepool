# Staff review: mechanisms, uncertainty and repair strategy

This is source analysis joined to retained execution evidence, not a claim that
every new failure was introduced by the last merge. Establish origin with a
controlled baseline if that distinction matters; chronology alone is insufficient.

## Acquisition closure: confirmed construction hole

The support object has several views of the same issued facts: acquired payloads,
selected rows, availability, maintained native facts, and Request projections.
Custody enrichment may replace an outer payload wrapper, but not every return
route closes these views against the resulting acquisition. Some callers repair
their own view later, leaving the nested support inconsistent. Other routes
repeat the merge/refresh work defensively.

That is the causal mechanism behind the reviewed source hole. It is not evidence
that every ArtifactIdentityMismatch has this cause. The robust repair is one
private consuming closure boundary, followed by one Request construction
boundary. It both removes repeated work and prevents caller-dependent repair.

The existing regression exercised one append/Request path; it did not cover
completion-only enrichment producing a third wrapper, immediate downstream use,
and all restoration/transition exits. Add those histories through actual issuers.
Do not generalize pointer equality into authority or make global interning a
precondition for validity. See leaf A.

## Prelude refusal diagnostic decision tree

Observed: four real notebook cases stop at the same initial public read with
main:Tidepool.Prelude → ArtifactIdentityMismatch. The earlier reexport name
conflict is gone. Root directly inspected the refusal producers and consumers.
The existing receipt collapses several different failures into this one reason.

Relevant production producers:

- compiler_projection.rs::checked_native_input_decision: missing selected row,
  missing Original slot, differing requested/role/entry Original or canonical
  artifact IDs, or a non-Original payload. These checks do **not** compare Arc
  pointers. A third wrapper with identical IDs alone cannot trigger them.
- certified_products.rs::authenticate_original_binding_membership: the exact
  receipt pair differs from the compiler primary pair, or its interface payload
  is not held. original_reference_payload_member also checks the exact owner,
  symbol, Original/interface IDs and the product's associated canonical interface.
- CertifiedSourceSelection::native_group_candidates chooses a canonical from
  its module map (or the continuation projection) before filtering retained
  candidates through select_input_native_group_candidates. The helper asks the
  continuation's owner-keyed input decision about the candidate's exact artifact.
- native_availability.rs::verified_root_choice separately consumes a direct
  root's carried decision. A direct root and an already-issued historical child
  do not have interchangeable authorization requirements.

Next step is ONE bounded observation at the actual rejecting branch, retaining:
phase/stage, request/input-owner identity, requested full binding and canonical
pair, current role pair, selected-row pair/payload kind, and whether the caller
is admitting a direct root or traversing an issued dependency. Carry a typed
predicate/stage tag through the error or existing opt-in trace; do not parse
strings or log every artifact. Inspect the retained producer receipt alongside it.

| Observation | Interpretation and repair direction |
|---|---|
| Role present, selected row/slot missing or stale after enrichment | Trace the first constructor that split those facts; leaf A's closed-acquisition boundary must prevent the returned state |
| Requested exact Original differs from current selected Original while traversing an issued historical child | Test whether current root policy is being reapplied to a historical edge; carry the existing exact dependency authority through that traversal, keeping direct-root checks strict |
| Canonical selected from a module-name map differs from the canonical paired with the issued Original | Stop reconstructing a pair from a broader/current inventory; carry the pair already issued by its owner |
| Exact fields agree but canonical/product membership differs | Inspect captured sidecar/interface custody and canonical identity at the actual issuer; do not substitute byte equality or silently reissue a witness |
| Wrong payload/absent receipt, or a genuinely unissued direct root | Refusal is correct; repair the producing path or fixture, never grant from retained custody |

The historical-child and owner-map rows are hypotheses suggested by actual
consumer code, not a diagnosis of this Prelude run. The morning compiler trace
contains Prelude hydration and source-frontend activity, but the emitted error
does not expose the rejecting predicate/pairs. A memo miss is not proof of
version skew. The retained first notebook case directory is recorded in
morning-review55-results.json; its full compiler trace and artifacts remain.

Once the cause is established, use a production-created valid history plus a
nearby wrong-pair or unissued-root control. Require the actual notebook read to
pass; a component test of the intended repair does not establish causality.

## HOME classification: confirmed information loss

The index answers “some payload for this module is held.” A consumer asks “is a
HOME implementation/interface held?” PackageCode was included in the first
answer and silently treated as affirmative evidence for the second. This is a
semantic type erasure at an owning query boundary, not an over-strict guard.

Keep typed category counts in the existing index. A pure independent census
over generated ownership histories detects drift, while the actual classifier
tests its interpretation. Generic observation retains its broader meaning.
The specific captured-A module remains to be confirmed; see leaf B.

## Stack overflow: confirmed overlap, incomplete latest suffix

The measured older chain has large nonrecursive native poll frames alive at once.
Heap-owning futures reduced some parent storage but left completed preparation
frames on the call stack while later initialization/publication polled. The
latest run progresses further and can add a new expensive checkout suffix.

The elegant fix is a real phase handoff: finish/drop preparation before beginning
stabilization, while one owner continues to hold cancellation and cleanup. It
removes an obsolete live frame instead of increasing a limit. The exact latest
first-fault observation determines whether that cut covers the new suffix.
Separate constructor, poll and synchronous payload stack costs. See leaf C.

## Cold test: confirmed projection mismatch

The test equated a visible facade compiler context with selected implementation
authority. Production intentionally hides session implementation roots there.
Changing the expected list to empty would make a weak test pass; promoting the
hidden implementation would distort production policy. Check the accepted export
and its actual issued native closure, then execute after cold recovery and output
removal. The final 41099 result distinguishes old R→H=41 from new H=99. See leaf D.

## Query fixture: confirmed diagnostic type mismatch

The local diagnostic uses String-returning show/list concatenation. The installed
unlines accepts [Text], and the final Prelude error accepts String. Therefore
the error-reporting branch itself fails whole-cell typechecking even when the
success branch would be taken. This is not evidence that hot queries failed.

Keep Tidepool's JIT-safe show; qualify Prelude.unlines in this String diagnostic.
Compile and execute the exact fixture, including its deliberately wrong-answer
arm. This escaped Rust compilation because include_str only embeds bytes. The
production-front-door Haskell test is the necessary detection layer; no separate
mini-framework or global Prelude policy change is required.

## Why this wave needs a different closing practice

Local source review and focused components repeatedly preceded the first joined
hosted consumer. Information was repaired at several downstream points instead
of closed once by its owner. Some tests asserted a convenient representation
rather than the contract they intended to preserve. “Build succeeded” also
covered fixtures that had not yet been typechecked by the actual compiler.

Close each owner, replay one representative joined history early, preserve the
intended refusal alongside the valid case, and only then accumulate dependent
parcels. Use one current gate ledger with exact revisions. This is the practical
prevention strategy; another list of reminders cannot substitute for these
constructor boundaries and executable histories.
