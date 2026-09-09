# Coordinate through shared values

Haskell is the swarm's working language. A tool block coordinates machines and
agents; it is not automatically a human-facing code sample or durable narrative.
Reuse bound values, functions and project names. Author a small sum type or helper
when repeated consequential distinctions justify it. Keep the underlying request,
progress and fork operations available; no universal message schema is required.

## Audience and recoverability

Actor-to-actor traffic is the minimum recoverable delta. Exact refs, fragments,
identifiers and expressions are welcome. Operator explanations remain readable.
An inter-agent final reply can be compact too; retain its real evidence for later
human summaries. Routine route/status events need no narration.

A recipient can recover its original assignment, inherited context, frozen shared
guidance and artifacts at accessible exact refs. It cannot recover decisions made
after its fork, another actor's uncommitted files, private custody observations or
changed authorization merely from a name. Forward those changes or a usable
artifact reference explicitly. A fresh selected context needs enough initial
meaning to interpret its refs; it does not inherit the caller's conversation.

Before sending, delete repeated standing guidance and recoverable rationale.
Keep what affects the next action or avoids a likely clarification/repair turn.
Do not compress away uncertain admission, authority, identity, custody or scope.
There is no fixed report format or required shorthand dictionary.

## Work and questions

`Candidate` already names source, actual checks and remaining gates. Successful
incorporation is evidence, not a `Question`. The supplied work/review helpers use `WorkProgress` for
both evidence and unresolved decisions; select that type explicitly with
`childWithProgress @WorkProgress @Delivery`. The worker can then publish:

```haskell
import qualified Tidepool.Actor as Actor
reportProgress (WorkProgress [candidate] open)
```

`open :: Attention` contains only decisions/blockers needing this Sol recipient's
action. Do not invent questions to announce activity or repeat a known gate.
Publish the current unresolved set so a newly attached collector can capture it;
attached collectors receive every later publication. Bound values keep this small; use `progressSummary` or `attentionSummary`
when inspecting rather than expanding every field. Keep original values for checks.

The Sol owner resolves ordinary interfaces and ownership. A reservation without
an executing owner is work to allocate. The planner has no execution subscription
after the initial understanding check. For a hard question, the Sol owner calls
`consultDesign` directly, using a fresh selected Astra and the returned watch:

```haskell
let question = (designQuestion task candidate "Which live roots own these wrappers?")
      { questionAlternatives = ["trace wrapper roots", "retain until retirement"] }
(expert, answerReady) <- consultDesign slot question
```

`slot :: DesignSlot` selects the plan section, unique group/labels, Astra model
and effort. The packet derives exact source and checks from the bound candidate.
Add only evidence and alternatives needed to decide. Inspect the actual answer;
incorporate any proposal before treating it as a checked decision. Keep useful
experts available for follow-up; do not kill in-flight work to meet a token target.

## Compact steering

For an active owned `response`, these are examples of style, not literal facts to
send in another project. Replace refs with accessible actual source/artifacts:

```haskell
correction <- updateRequest response "contract/native.md: private relay; no public schema. Own relay + v1 vectors; reject stale generation/canonical mismatch. Medium."
```

```haskell
correction <- updateRequest response "16@1 cancelled; provider Active; cleanup unknown. Retain bound worktree + socket/build artifacts. Same owner inspect settlement; no duplicate/teardown."
```

```haskell
correction <- updateRequest response "Envelope: mutable mode/target behind cached digest. Privatize; validate persisted reconstruction; reject mismatch before admission."
```

Handle `Left` and retain the returned receipt. Poll when its state changes your
next action; do independent work rather than repeatedly checking presentation.
Checked incorporation evidence can answer the engineering question without a
separate ceremony around every intermediate transport state. Preserve unresolved
transport uncertainty when it matters. No acknowledgment narrative is required. An uncertain
update must not become a new queued assignment. An already checked
`decision :: AcceptedDecision` has a convenience path with the same receipt:

```haskell
correction <- updateDecision response decision
```

## One local router per wave

`followWork` consumes each fixed response/progress pair in one typed actor. It
retains candidate evidence, the current questions per source, closure and the full
terminal response receipt. Publications arrive in order without rearming. Same
question keys in different sources stay distinct. Closing progress does not clear
questions or substitute for the later terminal response.

```haskell
owner <- actorContext
let sources = [("api", forkedResponse api, apiProgress), ("ui", forkedResponse ui, uiProgress)]
wave <- followWork sources (notifyWork owner (withCheckpoints (workMessage deliverySummary)))
```

Here both workers return Delivery. A wave of `Outcome Candidate` or review results
uses that result type and its own concise rendering. `workMessage` sends only new
or changed questions, resolved questions, source failures and final outcomes.
Adding one question does not repeat the others. Closing a source is quiet; resolving
the final question still messages its delta. The component policy above adds withCheckpoints: a new usable partial commit
wakes its integration owner before terminal delivery, while repeated evidence is
quiet. Simultaneous question changes remain in the same message. Local review
routers omit this decorator when candidate progress is only evidence for the
ongoing review. Publish checkpoints that the recipient can use, not every build
observation. All evidence remains queryable.
No mandatory report format: change the renderer to suit this recipient.

```haskell
view <- Actor.call wave WorkSnapshot
inspectFull [(sourceName s, progressSummary (sourceProgress s), sourceStatus s) | s <- collectedWork view]
```

Expand a relevant sourceResult for its actual response, execution and worktree
receipt. Notification attempts are retained in workNotices. Right contains the
admission receipt; Left contains the typed send failure. Receipt polling belongs
to the issuing actor, so when presentation affects a decision, use
`Actor.call wave (WorkNotification receipt)` while that sender is live. Calling
pollNotification directly in the parent does not acquire that authority. After
replacement, old receipts remain evidence; querying them through the new
incarnation can return NotificationUnauthorized. Use actual incorporation or the
owning inbox evidence instead of retrying an uncertain message.
A failed or unconfirmed send never erases the newly collected questions, and the
router does not retry it when the same questions arrive again. Replacing the
handler retains these outcomes. A lost notification is not proof that the worker
stopped: inspect retained state and actual delivery before choosing an intervention.

## Route typed values up the tree

A subtree can use a different sink: `WorkEvent Delivery -> Eff ...` is ordinary
Haskell, so forward checked component results to the parent's typed mailbox while
keeping partial evidence local. The parent can receive other event types in its
own protocol and select only relevant engineering decisions for its Sol owner.
The [executable handoff](../checks/handoff-router.hs) builds that parent mailbox,
casts the original WorkFinished value with its full response receipt, and retains
partial evidence in the child collector. Its [recipe](../Project/RoutingChecks.hs)
checks both later final heads and merges their real Git source. No Text encoding
or model wake sits between the two Haskell actors.

Choose sinks for all consequential outcomes: unresolved child questions need their
local owner, and unavailable/Blocked results need an action owner. The handoff
example's terminal-only sink is for a subtree whose local Sol already owns its
questions; it must not be copied as a policy that ignores every question.

Known already-authorized continuations can use a finite `route` to submit a
request when its prerequisite settles. The [review continuation](continuation.md)
combines that operation with typed actors, including retention of callback-created
handles. No callback awaits a busy model or confuses a candidate with acceptance.

## Retire a wave deliberately

Keep the collector through partial checkpoints and wind-down: later final results
still matter. Source closure does not imply that unresolved questions, uncertain
notifications or resource custody can be discarded. Incorporate the useful results
and put remaining obligations with a concrete owner, then:

```haskell
Actor.drainActor wave
finishedWave <- Actor.awaitExit wave
```

Drain closes admission and processes accepted messages; finishedWave retains the
exit and its state. New source handles belong to a new wave router. Retain useful
native workers independently for follow-up. Do not accumulate a live collector for
every completed review attempt.

For a behavior bug, keep the same ordered source names and handles and replace:

```haskell
wave <- Actor.replaceActor wave (workDefinition sources correctedSink)
```

Expected notification failures are retained values, not handler exceptions.
An arbitrary exception in custom code still pauses the actor: the runtime retains
the failed event, last committed state and queued work and alerts the supervisor.
Inspect that failed event when reconciling: replacement skips it, so an effectful
handler must not assume its failed update committed or replay an uncertain send.
Use total source projections and return sendMessage's Either through the sink.

## Spend model turns on decisions

For a compact view of ongoing work and abnormal terminals:

```haskell
current <- snapshot
inspectFull (actorSummary (workingAndAbnormal current))
```

Rows contain identity, label, lifecycle, provider health/staleness and current/
queued request IDs. The filter retains failed/cancelled actors and uncertain
stopped-provider observations; it omits idle retained actors without outstanding
requests or stale observations. The original snapshot retains all fields. This
is an inspection convenience, not proof that omitted resources are safe to retire.

Write the continuation once: collect streams, normalize repeated state, project
what this recipient needs, and route the known next action. Let Haskell execute
that logic between turns. Do not reproduce it as a cycle of polling, merging
lists, inventing watch labels and narrating unchanged gates. Use one persistent local wave router for progress and results. A finite watch
still suits an isolated consultation or a join whose decision requires all results.

The wave router retains the current questions; it does not resolve them or
invent issue identities. The source's owner publishes its updated cumulative set.
Candidates, checks and known product gates remain evidence, not synthetic questions.
Use compact projections at the decision boundary, preserving original evidence.
No subscription needs to wake the planner after the initial plan review.

The current watch transport can still deliver a notice after its handle was
polled. An already handled notice needs no repeat work. This package does not
claim to retract such notices or supply automatic update-presentation receipts;
those require changes in their owning delivery mechanisms. Do not build another
polling model or competing notification store around that limitation.

## Partial handoffs

For example, using actual accessible refs in place of these placeholders:

```text
ready tp:<commit> plans/lane-checkpoint.md; native:<commit>.
retained 33@1:<worktree> dirty bridge; owner finishing checkpoint.
gates lost-ack/native join; no acceptance. Next: rebase task branch onto main:<commit>.
```

Committed work, dirty retained work and open acceptance gates remain distinct.
The message points to durable evidence; it does not reproduce every receipt or
require a manifest type. New commits arriving during wind-down advance the handoff.
Keep them on task branches until their product acceptance permits main integration.
