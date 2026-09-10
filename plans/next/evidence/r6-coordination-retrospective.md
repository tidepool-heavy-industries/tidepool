# r6 coordination evidence

Condensed historical audit and interview evidence for the
[next coordination pass](../coordination-rsi.md). Disk incidents have separate
acceptance evidence and are excluded here. This is not a token/cost benchmark.

## What the auditors actually saw

Sol auditors read retained full histories. Applications covered six panes and two
reconstructed dead workers; engine covered its lead and six mechanism/review workers;
coordination covered root/coordinator and the introspection lead's complete persisted
rollout. Some collapsed tool bodies and failed worker history were unavailable.
No normalized provider trace was used, so no cache-hit or token-saving claim follows.

- Root Astra performed six visible initial actions and commissioned Sol, then
  idled. Later cleanup spam was runtime routing, not ordinary planner self-polling.
  Main's supervisor-recipient repair addresses that separately.
- Coordinator Sol had 179 visible actions, including 86 Haskell calls. It did real
  integration and caught semantic defects. It also repeatedly expanded cumulative
  snapshots, including an 871-line `workNotices` dump after guessed renderers failed.
- Applications repeated handle/category errors: admission `Either` versus receipt,
  unshowable handles, typed actor versus `Route`, `Eff` versus `Await`, absent
  qualified exports/progress bindings, and multi-repository prose used as a Git ref.
- Engine had progress type-application repairs and duplicate watches alongside
  collectors. The lead said polling was often result-access friction/reassurance;
  it had incorrectly treated a separate watch as more authoritative than `sourceResult`.
- Introspection selected the wrong similarly named watch after a notification,
  repeated pending polls and produced increasingly error-prone Haskell after context
  grew. Its parent also overlapped child-owned files, a separate decomposition issue.

These findings support removing mechanical turns. They do not support removing
independent review or source/custody checks: reviewers found real defects, and the
product handoff remained partial.

## Three rounds with the core actors

The stood-down Astra planner, Sol coordinator and Sol engine lead answered an
initial friction interview and two concrete design critiques. No implementation
workers were launched by these interviews.

- Astra recommended useful handle bundles and arbitrary typed outcomes, preserving
  the short observer path. It identified an actor owning a declared continuation as
  the strongest underused capability and separated collector completion from worker
  retirement and request ownership from effect membership.
- Coordinator identified the applications/engine/introspection checkpoint join as
  a real opportunity to eliminate repeated snapshot/watch/forward/drain calls.
  Integrating divergent histories still required model judgment. It challenged
  latest-candidate supersession and submission before state commit.
- Engine described a claimed candidate hash differing from its actual submitted
  worktree receipt, and a delayed provider failure after settlement. Both source
  values must remain available; custody can change after a request observer finishes.
  Its immediate continuation needed requests and a return address, not broad
  automatic launch/retirement powers.

The proposal incorporates those corrections without adopting every suggested
mechanism. Typed mailbox admission, native presentation, handling and resource custody
remain distinct; a new LLM acknowledgment is not required for every typed forward.
Source/acceptance policy stays in project Haskell rather than a Rust hash-equality
rule. Automatic WIP commits, a parallel custody ledger and a global scheduler do
not address the main interaction problem.

## Verified owners and gaps

- [`Project/Routing.hs`](../../../examples/shoal-workspace/.shoal/Project/Routing.hs)
  retains full `ResponseResult`s but embeds old/new progress in notices, discards
  their publication cursor and requires external drain. Its count view is small
  but often insufficient.
- [`Project/Observe.hs`](../../../examples/shoal-workspace/.shoal/Project/Observe.hs)
  still expands cumulative candidates/checks/gates in several summary functions.
- [`Actor/Internal.hs`](../../../haskell/lib/Tidepool/Actor/Internal.hs) restricts
  handlers to two profiles without `Replies`. The current
  [review continuation](../../../examples/shoal-workspace/.shoal/checks/review-continuation.hs)
  therefore combines an owner-row route, extra mailbox and callback-created
  collectors. Reuse effect witnesses from
  [`Actors/Role.hs`](../../../haskell/actors/Tidepool/Actors/Role.hs).
- [`Actor.hs`](../../../haskell/lib/Tidepool/Actor.hs) retains typed exits and
  supports custom finite loops, but has no typed public self address. `pollExit`
  followed by `call` races; returning early does not prove admitted inputs drained.
- [`request/sources.rs`](../../../tidepool-actor/src/request/sources.rs) captures
  current values and attaches under the publication lock, preserving source order
  and already-sent events when connections retire. Cross-source order is the actual
  acceptance order, not a fabricated global order.
- [`request.rs`](../../../tidepool-actor/src/request.rs) preserves submitted requests
  when unsubmitted reservations abort. Reusing a label allocates another identity.
  Typed-handle recovery after submission/before state commit needs explicit coverage.
- [`resident_actor.rs`](../../../tidepool-actor/src/resident_actor.rs) owns cleanup
  admission/revision fencing and provider-idle checks. Compose that owner instead
  of inferring safe retirement from a ready reply in each caller.

Raw audit/interview files remain local under `/tmp/rsi-*`. This record preserves
the consequential findings without committing private conversation transcripts.
