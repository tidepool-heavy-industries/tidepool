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
incorporation is evidence, not a `Question`. Use `WorkProgress` when a caller wants
both evidence and unresolved decisions; select that type with
`childWithProgress @WorkProgress @Delivery`. The worker can then publish:

```haskell
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

## Independent progress without relay turns

`followAttentionSources` starts one persistent typed Haskell actor. It captures
the current values, consumes every later publication without rearming, and keeps
each named source's questions and closure separately. Equal keys across sources
remain distinct; source names must be unique. Order-only and duplicate questions
do not invoke the sink again. Source closure preserves unresolved questions.

The sink runs inside the collector with its own authority. It can cast to another
typed actor or send normal steering to a bound `solOwner :: AgentRef`. For example,
this policy messages unresolved question keys when the retained view changes:

```haskell
import qualified Data.Text as Text
collection <- followAttentionSources [("api", apiQuestions), ("ui", uiQuestions)] $ \state ->
  case [attentionSource source <> ":" <> questionKey q | source <- state, q <- attentionQuestions source] of
    [] -> pure ()
    keys -> do
      sent <- sendMessage solOwner (Text.intercalate ";" keys)
      either (error . show) (const (pure ())) sent
```

Choose the projection for the task; routine evidence need not wake a model.
A collector cannot call its creator's `reportProgress` using inherited reply
ownership. `followAttention` is the single-source convenience without a cursor.
For deliberate inspection, query the same retained actor:

```haskell
import qualified Tidepool.Actor as Actor
view <- Actor.call collection AttentionSnapshot
inspectFull view
```

A failed sink pauses the collector and notifies its supervisor; later messages
remain queued. Keep its source bindings and define a corrected sink, then use
the same primitive as for any stateful actor:

```haskell
collection2 <- Actor.replaceActor collection
  (attentionDefinition [("api", apiQuestions), ("ui", uiQuestions)] correctedSink)
view <- Actor.call collection2 AttentionSnapshot
```

The successor retains committed questions and queued events. The failed event is
kept as evidence and skipped; an uncertain send is not replayed. Keep source names,
order and handles unchanged. `correctedSink` is an ordinary Haskell function with
the same effect row as the original sink. Use the returned handle thereafter.
When all sources close, the collector remains available for inspection.
When finished with the current handle, `Actor.drainActor collection2` closes
admission; `Actor.awaitExit collection2`
explicitly waits for the retained final state.

Use independent result watches when each candidate can advance integration.
For coupled results, an applicative join is useful. Source integration is native
git followed by focused checks; it is not `integrateFork`, an acknowledgment, or
concatenation of worker reports.

For multi-lane handoffs, attach progress and settlement sources to one small
protocol after commissioning the lanes. Retain evidence locally and wake for
independently useful candidates, including partial ones. Later terminal results
must still reach the owner. Consuming a partial checkpoint
does not detach the collector or finish the coordinator's obligation. Keep the
collector until final heads are incorporated or remaining custody has an owner.
The executable [handoff router](../checks/handoff-router.hs) demonstrates this with
commit refs; [twoLaneHandoff](../Project/RoutingChecks.hs) consumes a partial update,
receives both later final heads and merges their real Git commits. Substitute the
task's `Delivery`/`Candidate` values and meaningful wake policy in your own protocol.

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
lists, inventing watch labels and narrating unchanged gates. Use a finite result watch
for an engineering join and a persistent collector for ongoing questions.

The source collector retains the current questions; it does not resolve them or
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
