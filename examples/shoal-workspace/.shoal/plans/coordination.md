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
State is cumulative because intermediate publications may coalesce. Bound values
make cumulative publication small; use `progressSummary` or `attentionSummary`
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

`followAttentionSources` follows named streams using `awaitAnyProgress`. It keeps
their cursors and last questions separately, ignores order-only/identical updates,
and retains questions when a source closes or rejects observation. Equal question
keys in different sources remain distinct. Source names must be unique.

For an owner activated with progress type `[AttentionSource]`, and two bound child
progress handles, this forwards retained execution state to its Sol requester:

```haskell
collection <- followAttentionSources [("api", apiQuestions), ("ui", uiQuestions)] reportProgress
```

It does not wait for the slower source, construct a model message or notify Astra.
The sink is ordinary Haskell: choose a projection, another known continuation or
publication to the appropriate owner. Sink failure remains a failed retained route;
inspect `listRoutes`/`pollRoute` before attempting recovery. `followAttention` is
the single-source convenience. Do not mistake completion of its first route for
completion of subscriptions subsequently installed by callbacks.

Use independent result watches when each candidate can advance integration.
For coupled results, an applicative join is useful. Source integration is native
git followed by focused checks; it is not `integrateFork`, an acknowledgment, or
concatenation of worker reports.

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
lists, inventing watch labels and narrating unchanged gates. Use a result watch
for an engineering join, and question watches only for unresolved action.

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
