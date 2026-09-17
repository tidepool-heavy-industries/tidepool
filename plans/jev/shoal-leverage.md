# Jev as Shoal's branch predictor: where the leverage is

Design brainstorm, 2026-09-16. Assumes the typed single-operation Jev effect
exists. Not a plan of record; a ranked argument about which workflows to
reshape and how to know if the reshaping worked.

## The substrate this assumes

Shoal's primary interface for a model is a Haskell notebook. A turn is a
runnable program over previously defined values and types, and the typed
values are the RPC channel between agents: define a record, ask another agent
to produce one, and it writes Haskell over Tidepool to do so. Two consequences
for everything below:

- Jev is a library value an agent writes into a cell, not a harness service.
  Every capability here is a combinator or an actor sink the authored program
  composes, and the program stays inspectable in the notebook.
- A typed request between agents is a record with a shape. Some fields are
  judgment-shaped: an enum, a boolean, an ordered level, a selection from
  supplied candidates. Those are exactly Jev's three primitives. A request can
  therefore be partially answered by a judgment snapshot over already retained
  state before a model is woken to fill the generative fields, and sometimes
  there are no generative fields left.

## System 1 in a notebook: the model turn is the fallback

The cell runs in a GHCi-style machine session whose bindings persist. That
changes what "fallback" means. When an authored decision tree runs out, the
cell does not fail and nothing restarts: it returns a typed outcome whose
constructors include "I do not know, here is everything I found", and the
current model turn, which was going to run anyway, resumes over the values
still resident in the notebook.

```haskell
data InvestigationOutcome
  = Located Witness Evidence
  | NeedsJudgment Inquiry Evidence Alternatives
```

`NeedsJudgment` is not a low-confidence exception. It is the normal way a
cell ends when it reaches a distinction it was not written to make:

- both routes look useful and choosing needs a design preference;
- the available candidates do not cover what the evidence suggests;
- two differently framed judgments disagree;
- the next action exceeds what this cell was authored to decide.

Every packet therefore carries explicit exits alongside its semantic
alternatives: a `defer_to_model` key on Choices whose resolution may need
preference, and a `no_candidate_covers_evidence` key wherever coverage
matters. Policy also derives handbacks from near-ties and from disagreement
between sibling judgments. Calling a specialist is another explicitly
authored branch; returning to the current model is the simplest one and needs
no new mechanism.

Two consequences follow. First, none of this must run unattended; the target
is a model that writes ordinary Haskell with semantic choices embedded,
executes as far as that program usefully goes, then thinks again with its
intermediate results intact. Second, a recurring handback reason is a signal:
the model can extend the resident helper with a new branch, and the next
invocation gets further. The decision tree grows by turning System 2
resolutions into System 1 branches, in the same session, without anyone
shipping a release.

This also changes the evaluation question from "did the cell resolve it" to
"how much useful work did the cell do before handing back, and how well
prepared was the handoff". A cell that reliably performs three investigation
steps and returns one precise unresolved question with its evidence is
already succeeding.

## Where Shoal wastes intelligence today

Reading `Project.Swarm`, `Project.Routing`, `Project.Work`, and `Project.Observe`,
the recurring sinks are concrete:

1. **Parent wakes for routing.** `advance` emits `WakeParent` for `FrontierReady`,
   `BatchSettled`, `ChildNeedsDecision`, and `IntegrationNeedsDecision`. The
   parent is a planner-tier model that reads `view` and mostly decides "does an
   accepted decision already answer this" or "is this blocking". Every
   `ContractQuestion` from a reviewer stops the child and wakes the parent.
2. **Work notifications on every question delta.** `notifyWork` wakes the owner
   whenever `openedQuestions` is nonempty. Deduplication is `sameQuestion`, which
   is key equality. A reworded duplicate wakes the owner again.
3. **A specialist per design question.** `consultDesign` forks a
   specialist-model coding agent for each `DesignQuestion`. Nothing checks the
   question against retained `AcceptedDecision`s beyond key match.
4. **Full review and full checks for every candidate.** `CheckAndReview` runs the
   configured checks and forks a fresh reviewer for every `Produced` revision,
   regardless of whether the change is a doc edit or a contract change. `Fix`
   findings that are contract-level cost a repair round before the reviewer
   finally says `ContractQuestion`.
5. **Executor turns spent reading tool output.** A notebook cell today is one
   tool result per model round: run test, read, choose diagnostic, read source,
   choose caller, read, choose test, run. Each choice is a frontier round.
6. **Context by concatenation.** `taskContext` renders every accepted decision;
   `reviewContext` and `designContext` render everything they have. Agents start
   from irrelevant context and rediscover which parts matter.
7. **Late discovery of sufficiency and contradiction.** Nothing notices that the
   evidence a worker has retained already answers its question, or contradicts
   its assumption, until a model reads it.

The common shape: a deterministic state machine reaches a point where the next
step depends on a semantic distinction, and the only mechanism available is to
wake a model that then reads the whole state. Jev lets the state machine make
the distinction itself, and lets the model wake only when the distinction is
genuinely open.

## Seven capabilities

### 1. Decision memory: questions answered before they reach a model

**Current workflow.** Worker raises `DesignQuestion` → `WorkChanged` → owner
wakes, reads it, recalls or searches accepted decisions → either replies with
`decisionContext` or calls `consultDesign` → specialist forks, reads plan and
source, replies → owner incorporates. Two to three planner rounds plus one
specialist session, per question, including duplicates.

**Jev-native workflow.** Retention is the semantic boundary, not the wake.
When a question is retained, Haskell gathers the candidate pool: every
`AcceptedDecision` on the current source ancestry plus every open `Question`
across sources. One packet judges the new question against the pool. Policy:
"answered" with margin → deliver `decisionContext` evidence to the worker
deterministically, no wake; "duplicate of open question" → attach to the
existing question, no wake; "conflict" → wake the owner with both records;
"distinct and blocking" → `consultDesign` directly, without waking the owner
first; "distinct, not blocking" → queue for the next ordinary wake.

**Packet.** State: the question record, the decision pool keyed by decision
name with scope, source, summary, evidence, and the open-question pool.

- Choice `governing`: keys into the decision pool plus `none_governs`.
  "Which retained decision, if any, decides this exact operation and failure
  condition at the question's source revision?"
- Choice `relationship_to_open`: keys into the open-question pool plus
  `no_related_question`.
- Noul per decision `pool.<d>.applies_at_revision`: "Does `d` still apply given
  its source and the question's source, or has the premise changed?"
- Noul `contradicts_accepted`: "Does the question's finding contradict any
  decision in the pool?"
- Noul `blocks_obligation`: "Can the worker's obligation proceed without an
  answer?"
- Score `answer_kind`: local evidence suffices / needs a specialist reading /
  needs a project decision.
- Premise-prefixed: "If `governing` is `d`, does `d.summary` need amendment to
  cover this finding?" for the top candidates, so the reply can be a
  `withDecision` delivery or an `AmendPlan` without a second call.

**Jev selects.** An existing `AcceptedDecision` value, an existing `Question`
to merge into, or nothing. Delivery uses the existing `updateDecision` path.

**Why heuristics fail.** Key equality misses "who owns retries" versus "does the
transport or session rerun a timed-out request", and it merges nothing that is
worded differently. Applicability across revisions is semantic: a rename does
not invalidate a decision, a changed contract premise does.

**Savings.** Every duplicate or already-answered question saves one specialist
session and one to two planner rounds. In a swarm with five workers on one
plan, expect a third of questions to be answered or merged this way.

**Failure policy.** `governing` margin under a configured gap or
`contradicts_accepted` above 0.3 → wake owner with the packet attached. A
delivered answer is evidence, not authority: the worker's next progress still
flows through the sink, and the delivery is journaled with the distribution.
`none_governs` at high confidence plus `blocks_obligation` → consult directly.

**Evaluation.** Replay `workHistory` from real runs: every `WorkChanged` with
opened questions, labeled by what the owner actually did. Measure agreement
with the human-labeled disposition, false "answered" rate, and specialist
sessions avoided. Justified if false-answered under 5% and a quarter or more of
questions handled without a model.

### 2. Wake economy: the parent stops being the router

**Current workflow.** `advance` produces wake reasons; the parent wakes per
reason or per coalesced batch, reads `view`, and decides what to do about each
`NeedsDecision` child, each merge notice, and each frontier-ready signal. Many
wakes end with "continue" or "ask the reviewer again".

**Jev-native workflow.** `WakeParent` becomes a proposal, not a command. Before
waking, Haskell prepares the batch view plus the stopped child's reason, the
child's task, accepted decisions, and the retained evidence. One packet judges
each pending reason. Policy: reasons that are "mechanical continuation" are
executed by Haskell (re-issue with a widened repair budget, queue the notice,
deliver an accepted decision); reasons that are "genuine decision" are batched
into one wake with a prepared summary; delay-cost scoring decides whether that
wake is now or at the next checkpoint.

**Packet.** State: `view`, the reasons, per-child phase and budgets, task
obligations, accepted decisions, the last review findings.

- Each reason: Choice `disposition`: continue_mechanically / deliver_existing_
  decision / batch_for_next_wake / wake_now / halt_integration.
- Each reason: Score `delay_cost` with four concrete levels.
- Noul `reasons_share_root_cause`: "Do the pending reasons stem from one
  underlying problem?"
- Choice `primary_reason`: keys into reasons. "Which reason, if resolved, most
  likely resolves the others?"
- Premise-prefixed: "If `primary_reason` is a contract question, does an
  accepted decision already settle it?"

**Jev selects.** Existing `Request` values from `Project.Swarm` (re-issue,
queue notice), an `AcceptedDecision` to deliver, or a wake with a chosen subset
of reasons.

**Why heuristics fail.** Whether "repair budget exhausted: reviewer disputes
error-type naming" is a project decision or a mechanical widening is a
semantic call. Whether three children stopped for one root cause is not visible
in phases.

**Savings.** Fewer, denser parent wakes. Each avoided wake is a planner round
plus the model rereading the batch. Root-cause batching also means the parent
decides once instead of three times.

**Failure policy.** Any `wake_now` above 0.5 wakes immediately with everything.
Halt reasons (`MergeUncertain`, integration failures) never go through the
filter; they always wake. Continue-mechanically requires margin over 0.4 against
the next disposition and is capped by existing budgets in `Limits`, which Jev
cannot widen.

**Evaluation.** Replay recorded `WakeParent` sequences; label each with what
the parent did. Measure wakes avoided, decisions delayed past a labeled
deadline, and wrong mechanical continuations. Justified if wakes drop by a
third with zero wrong continuations on halt-class reasons.

### 3. Semantic execution layer: one cell, many tool operations

**Current workflow.** Executor round: run the failing check. Round: read the
log, pick a diagnostic. Round: read source at its span. Round: pick a caller
from references. Round: read it. Round: pick and run a test. Round: interpret.
Seven rounds, each carrying the growing transcript.

**Jev-native workflow.** The executor authors one cell: a bounded microprogram
that runs the check, parses diagnostics into candidates, asks one packet, reads
the selected span, enumerates references, asks a second packet at that new
evidence boundary, runs the selected test, and returns a typed
`CellOutcome` with the witness, the distributions, and the trace. The executor
sees only the outcome. The cell shape is `Project.Work` style Haskell over
existing `Cmd` values; Jev never sees a shell string.

**Packet one** (after the failing check). State: inquiry, diagnostics grouped by
compiler identity, the source neighborhood of each group's span.

- Choice `explaining_diagnostic`: keys into groups plus `none_explains`.
- Each group: Noul `is_consequence`: "Is this a downstream consequence of
  another listed diagnostic?"
- Noul `source_visible_suffices`: "Do the supplied neighborhoods already show
  the mechanism?"
- Score `next_read_breadth`: one span / one file / callers of one symbol /
  broader.
- Premise-prefixed per top group: "If `g` explains the failure, which
  relationship kind is most useful next: callers, callees, or type
  definition?" as a Choice.

**Packet two** (after the selected read and reference listing). State: the
inquiry, the read span, the edges pool with relation kind and nearby source,
available tests with what they assert.

- Choice `next_edge`: keys into edges plus `stop_with_witness` plus
  `no_useful_edge`.
- Each edge: Noul `relevant`.
- Choice `discriminating_test`: keys into tests plus `no_test`.
- Noul `current_span_is_witness`.
- Noul `evidence_contradicts_inquiry_premise`.

**Jev selects.** Diagnostic groups, LSP edges, test command values, and the
stop condition. All are existing typed candidates; the continuation closures
are retained per candidate.

**Competing explanations can survive execution.** This is an available
pattern for open investigations, not the mandatory shape of every cell; the
common cell is one judgment and one command. When packet one splits between
two mechanisms, say cancellation and duplicate delivery at 0.45 and 0.40, the
cell does not commit. Haskell retains both hypotheses, selects one
discriminating observation per hypothesis from the premise-prefixed answers
already in the packet, gathers both, and asks packet two over the enriched
state with the same hypotheses as alternatives plus "neither". Ambiguity
becomes a reason to collect targeted evidence inside the cell rather than a
reason to return. The frontier model then receives either one supported
mechanism with the discriminating evidence, or two mechanisms with the
evidence that failed to separate them, which is a better investigation than
an early commitment to one story. The read budget bounds how many hypotheses
stay alive; two or three is the normal case.

Illustrative shape, with each judgment selecting retained typed continuations
and ordinary Haskell carrying the investigation across Bash, LSP, and later
judgments:

```haskell
investigate inquiry = do
  diagnostics <- runAndParse check
  focus       <- judgeDiagnostics inquiry diagnostics
  frontier    <- inspectSourceAndReferences focus
  routes      <- judgeFrontier inquiry frontier          -- may keep several alive
  evidence    <- traverseSelected routes                 -- one observation per route
  assessment  <- judgeEvidence inquiry evidence          -- over the enriched state
  pure (retainEvidence assessment evidence)
```

**Why heuristics fail.** "Which diagnostic is the cause rather than the
cascade" and "which caller determines whether the reply is published" are
semantic reads of source, not string matches.

**Savings.** Seven executor rounds become one authoring round plus one
interpretation round, with two Jev calls in between. Tool calls are the same or
fewer, since the packet prunes reads. The transcript the executor carries is
the outcome, not the log.

**Handback policy.** `none_explains`, `no_useful_edge`, or `defer_to_model`
winning → the cell returns `NeedsJudgment` with the inquiry, the evidence
gathered so far, the live alternatives, and their distributions; the current
model turn resumes over the resident bindings. A near-tie on `next_edge`
within the read budget → keep both alive and gather discriminating evidence;
past the budget → hand back the tie with both branches' evidence. Sibling
judgments that disagree (a witness Noul high while `next_edge` prefers to
continue) → hand back with both. Budget exhaustion is a typed return, never a
retry. Every packet and selection is retained behind the outcome's trace ref,
and the handback reason is journaled so recurring reasons can become new
branches.

**Evaluation.** Thirty real investigations from journals with an
independently checked witness, not the witness the recorded agent settled
on. Branches the recorded session never took must be actually fetched, not
scored from the transcript. Compare witness correctness, executor rounds,
bytes of transcript, and wall time against both the recorded session and a
fixed search recipe. Justified if correctness is at least the recorded
session's at under half the rounds.

### 4. Adaptive review and pre-annotated phase transitions

**Current workflow.** `Produced` → `CheckAndReview` runs all configured checks
and forks a reviewer → `Fix` → `RepairWork` → `Produced` → checks and a new
reviewer again → sometimes `ContractQuestion` on the second pass → stop → wake.
Two reviewer sessions, one implementer repair, one parent wake, for a candidate
whose premise was unsettled from the start.

**Jev-native workflow.** On `Produced`, before issuing `CheckAndReview`, Haskell
prepares the diff summary by owned path, the acceptance text, claimed checks,
remaining gates, and accepted decisions. One packet annotates the candidate.
Policy chooses the check subset and review depth, and pre-decides how each
likely reply will be handled. On `Reviewed (Fix findings)`, a second small
packet classifies the findings so contract-level findings go to decision
memory (capability 1) instead of a repair round.

**Packet** (on `Produced`). State: task, diff by path, claimed checks,
configured checks with what they cover, accepted decisions.

- Noul `touches_outside_owned_paths`.
- Noul `claimed_checks_cover_acceptance`.
- Noul `premise_unsettled`: "Does the change depend on a contract not decided
  in the accepted decisions?"
- Score `change_breadth`: mechanical / local behavior / crosses a contract /
  affects unrelated consumers.
- Choice `first_check`: keys into configured checks. "Which check most directly
  exercises the acceptance criterion?"
- Each check: Noul `worth_running_before_review`.
- Premise-prefixed: "If review returns Fix, is the likely finding mechanical
  (naming, missing test) or contractual?" as a Choice.
- Premise-prefixed: "If the merge requires rebase, do the changed paths overlap
  the paths changed on the new base?" using the base diff summary.

**Packet** (on `Fix`). Each finding: Choice `kind`: mechanical / behavioral /
contractual / out_of_scope; Noul `repairable_without_decision`.

**Jev selects.** Existing check commands, a review depth from a small fixed
menu, and which existing path a finding takes: `RepairWork`, decision memory,
or `stop`.

**Why heuristics fail.** Path-based rules cannot tell a rename in an owned file
from a contract change in the same file. Whether a finding is a naming quibble
or a premise dispute is what the reviewer would otherwise spend a session on.

**Savings.** Predicted `ContractQuestion` skips one review and one repair
round. Mechanical-only candidates get the focused check and a lighter review.
Findings classified as contractual skip the repair round that would fail.
Expect roughly one reviewer session per two candidates saved, and repair
rounds down by a third.

**Failure policy.** Review is never skipped; depth is chosen. `premise_unsettled`
above 0.5 routes to decision memory first, then review proceeds with the
answer attached. `change_breadth` above the third level forces the full check
set. A finding at a near-tie between mechanical and contractual goes to repair
with the question attached, which is today's behavior.

**Evaluation.** Replay recorded candidate lifecycles: predicted finding kind
versus actual reviewer verdict, predicted `ContractQuestion` versus actual.
Justified if contract prediction precision is above 70% and reviewer sessions
per landed candidate drop by a quarter without more integration failures.

### 5. Investigation as infrastructure: Jev-steered hylo with run-ahead

**Current workflow.** A check fails; the parent or the implementer wakes and
begins investigating from the log. The investigation is model-driven, one hop
per round.

**Jev-native workflow.** Investigation becomes a `hyloM` over `Tidepool.Swarm`
with Jev in the `gated` and `budgeted` slots and in the algebra. The coalgebra
enumerates the exact neighborhood; a packet judges per-edge relevance and
whether the node is a witness; `gated` refuses descent on children below a
relevance floor; the algebra's packet judges whether children's evidence
supports, contradicts, or leaves a named gap. The fold returns an evidence
bundle with exact spans and revisions. Because this is cheap and needs no
model, it runs **ahead**: every failing check and every `Fix` triggers an
investigation before anyone wakes, so the wake carries a witness.

**Packet** (coalgebra, per node). As capability 3 packet two, plus Noul
`node_closes_inquiry` and Score `remaining_depth_worth`: none / one hop / a few
hops / unbounded. **Packet** (algebra, per fold). Each child: Noul
`supports_inquiry`, Noul `contradicts_inquiry`; Choice `primary_witness` over
children plus `no_witness`; Noul `gap_named_is_real`: "Does the missing item the
children name actually block a conclusion?"

**Jev selects.** LSP edges to expand, children to retain, the primary witness
span, and the stop.

**Why heuristics fail.** Breadth-first with a depth cap reads everything
reachable; name matching follows suggestive names. The semantic read of "this
gate publishes only when not cancelled" is the whole value.

**Savings.** Every investigation that lands on a witness saves three to six
model rounds. Run-ahead means the first model round starts at the mechanism
rather than at the log.

**Failure policy.** The hylo's budgets are deterministic and Jev cannot widen
them. `no_witness` at the root fold returns `NeedEvidence` with the retained
paths and gaps. Contradiction above 0.5 anywhere marks the bundle as contested
and the wake says so. Concurrent siblings use `hyloConcurrentM`; plan order
reassembly holds.

**Evaluation.** Known investigations with labeled witnesses. Witness recall,
spans read, wall time, and the fraction of failing checks where the run-ahead
bundle was used unchanged by the waking model. Justified if recall exceeds 70%
and the model's first round cites the bundle in over half of cases.

### 6. Context compiler: minimal evidence packets for every fork and wake

**Current workflow.** `taskContext`, `reviewContext`, and `designContext`
concatenate everything. The forked agent spends its first rounds working out
what matters.

**Jev-native workflow.** Every context construction site passes its candidate
pieces through one packet and assembles from the selected ones. The pool is
the retained evidence: accepted decisions, candidates, questions, recent
`WorkChanged` deltas, source spans from capability 5 bundles.

**Packet.** State: the assignment, the pool keyed by piece id. Each piece: Noul
`relevant`, Noul `supports_assignment_premise`, Noul `contradicts`, Score
`diagnosticity`. Choice `primary_witness`. Noul `pool_sufficient`. Noul
`semantic_gap_remains`.

**Jev selects.** Pieces to include, in an order Haskell fixes from
diagnosticity, with provenance retained.

**Why heuristics fail.** Recency and key overlap do not capture "this decision
is about the same contract under a different name".

**Savings.** Shorter first rounds and fewer clarifying rounds per forked agent.
Hard to attribute to a single round, but it compounds across every fork.

**Failure policy.** `pool_sufficient` below 0.5 attaches the gap statement to
the context so the agent knows what is missing. Contradicted pieces are always
included, flagged, never dropped.

**Evaluation.** Replay forks; measure first-round tool calls and rounds to
first substantive action against the recorded sessions, and whether any
labeled-essential piece was dropped. Justified if essential-drop is under 2%
and rounds to first action fall by one on average.

### 7. Continuous cheap supervision between expensive wakes

**Current workflow.** Supervisor wakes on batch signals only; between wakes,
duplicated effort, semantic blockers, and consequential interactions between
siblings go unnoticed until someone reports them.

**Jev-native workflow.** On a timer or on each progress event, Haskell prepares
`workingAndAbnormal` plus the work state and asks one packet. Policy queues
steering to workers, holds affected integration, and wakes the supervisor only
for consequential interactions.

**Packet.** Each worker: Noul `on_assignment`, Noul `duplicates_sibling`, Noul
`semantically_blocked`, Noul `advances_parent_goal`. Choice
`consequential_interaction` over enumerated sibling pairs plus none. Noul
`supervisor_wake_needed`. Score `coordination_urgency`.

**Jev selects.** Existing steering messages, hold flags in the batch, a wake.

**Why heuristics fail.** Duplicate effort and semantic blockage are invisible
in roster state.

**Savings.** Earlier redirection of duplicated work; fewer supervisor wakes for
non-consequential state.

**Failure policy.** Steering is advisory and journaled; holds use existing
`paused` semantics; a wake above 0.5 always wakes.

**Evaluation.** Replay actor-tree snapshots from runs where duplicate or
blocked work was later discovered; measure detection lead time. Justified if
duplicates are flagged a wake earlier than they were noticed.

### 8. Typed request triage: judgment-shaped fields filled before the wake

**Current workflow.** Agent A defines `data Verdict = Verdict { applies ::
Bool, severity :: Severity, owner :: Owner, reasoning :: Text }` and sends a
typed request to agent B: "fill one of these for candidate c". B wakes, reads
the candidate and the plan, writes a cell that constructs a `Verdict`, and
replies. One full model round on B for every request, even when three of the
four fields are determined by retained state.

**Jev-native workflow.** The request carries its type. A generic derivation
over the record maps `Bool` fields to Nouls, closed enums to Choices, ordered
enums to Scores, and `Candidates a` fields to Choices over the supplied pool,
with each field's documentation as the instruction. Haskell prepares the state
from what A already retains and asks one packet. Policy: if every field is
judgment-shaped and every answer clears its margin, the reply is constructed
and B never wakes. If generative fields remain, B wakes with the judgment
fields pre-filled and their distributions attached, so its round is "write
the reasoning and confirm or dispute", not "read everything and decide".
Disputes are typed: B's cell can overwrite a pre-filled field, and the
overwrite is journaled against the distribution it disagreed with.

**Packet.** Derived from the record. For the example: Noul `applies` from the
field doc; Score `severity` over the enum's documented levels; Choice `owner`
over the supplied owner pool plus `no_listed_owner`; premise-prefixed: "If
`applies` is false, which listed owner should be told?" so a negative verdict
still routes.

**Jev selects.** Field values that are members of closed types or supplied
pools. The generative field stays empty until a model writes it.

**Why heuristics fail.** The fields are semantic by construction: the record
author chose to ask a model because a rule could not fill them.

**Savings.** Every request whose fields are all judgment-shaped saves a full
model round on the recipient. Every mixed request shortens the recipient's
round and removes the reread. This is the most frequent shape in a swarm that
communicates through typed values.

**Failure policy.** A field below margin is left empty and the wake proceeds
with the partial record and the distribution. A recipient's overwrite of a
pre-filled field above a configured rate for that record type disables
pre-filling for that type until an operator looks. Requests whose type has any
field Jev cannot derive are never auto-replied.

**Evaluation.** Replay journaled typed requests and their replies. Measure
agreement on judgment-shaped fields, auto-reply rate, and overwrite rate.
Justified if field agreement exceeds 90% and a fifth of requests auto-reply.

## Three traces

### Trace A: a stopped child (capabilities 1 and 2)

Before. Reviewer returns `ContractQuestion "receiver dedup vs sender retry"`.
`advance` stops the child and emits `ChildNeedsDecision`. Parent wakes (round
1), reads `view`, reads the child's task and findings (round 2), recalls that
plan `delivery.md` accepted "receiver deduplicates stable ids" two revisions
ago, writes a decision context, re-issues repair (round 3). Three planner
rounds, one wake.

After. `advance` emits the same reason. The wake filter prepares the batch
view, the reason, the task, and the decision pool, and asks one packet:
`governing` = `receiver_dedup@r19` at 0.94, `applies_at_revision` 0.91,
`contradicts_accepted` 0.08, `disposition` = deliver_existing_decision at 0.88,
`delay_cost` = 2.1. Policy calls `withDecision`, re-issues `RepairWork` with
the decision context in the findings, journals the packet, and queues a
next-wake note. Zero planner rounds. One Jev call at about 200 ms.

### Trace B: a failing check (capabilities 3 and 5)

Before. Implementer's check fails. Round 1: reads the log, picks a diagnostic.
Round 2: reads the span. Round 3: lists references, picks one. Round 4: reads
it, forms a hypothesis. Round 5: picks and runs a test. Round 6: interprets and
patches. Six executor rounds.

After. The check failure triggers run-ahead: the investigation hylo unfolds
from the failing diagnostic's span, two Jev packets steer it to
`publish_if_active` at depth two, the algebra marks it as the primary witness
at 0.86 with no contradiction, and the bundle is attached to the wake. The
implementer's round 1 sees the witness span, the traversal path, the
distributions, and the discriminating test already selected. It runs the test
and patches in round 2. Two executor rounds, two Jev calls, and the tool reads
happened before the model woke.

### Trace C: a candidate lifecycle (capability 4)

Before. `Produced` → all checks → reviewer session 1 → `Fix ["error type naming
inconsistent", "premise: retries after timeout undecided"]` → repair session →
`Produced` → all checks → reviewer session 2 → `ContractQuestion` → stop → wake.
Two reviewer sessions, one repair session, one wake, before the real problem is
named.

After. `Produced` → packet: `premise_unsettled` 0.81, `change_breadth` 2.4,
`first_check` = `actor_retry_fixture`, predicted-Fix kind = contractual at
0.72. Policy routes the premise to decision memory first: `governing` =
`none_governs` at 0.9, `blocks_obligation` 0.85 → `consultDesign` directly.
Specialist answers; `withDecision` updates the task; then `CheckAndReview`
runs with the focused check first and the decision attached. Reviewer session
1 returns `Accept`. One reviewer session, one specialist session, zero repair
rounds, zero parent wakes.

## Ranking

Scores 1 to 5, higher is better for every column, including consequence of a
wrong judgment (5 means a wrong judgment is cheap to detect and reverse).

| Capability | Efficacy gain | Frequency | Semantic advantage | Observability | Wrong-judgment safety | Ease | Frontier rounds saved |
|---|---|---|---|---|---|---|---|
| 1 Decision memory | 5 | 4 | 5 | 5 | 3 | 4 | 5 |
| 4 Adaptive review | 5 | 5 | 4 | 4 | 3 | 3 | 5 |
| 3 Semantic execution layer | 5 | 5 | 4 | 4 | 4 | 2 | 5 |
| 5 Investigation hylo with run-ahead | 4 | 4 | 5 | 5 | 5 | 2 | 4 |
| 2 Wake economy | 4 | 4 | 3 | 4 | 2 | 3 | 4 |
| 8 Typed request triage | 5 | 5 | 4 | 5 | 4 | 3 | 5 |
| 6 Context compiler | 3 | 5 | 3 | 3 | 4 | 4 | 2 |
| 7 Continuous supervision | 3 | 3 | 4 | 3 | 4 | 3 | 2 |

Capability 1 ranks first because its judgments are checkable against retained
records, its wrong answers are visible as a delivered decision the worker can
dispute, and it removes whole specialist sessions. Capability 8 is close
behind and is the more general mechanism: decision memory is typed request
triage specialized to `DesignQuestion`. Capability 2 is powerful but has the
worst wrong-judgment profile: a suppressed wake is a silent delay.

## Conclusions

**Implement first: the authored investigation cell (3), then reuse it for
run-ahead (5).** This is Astra's reordering and it is right. The cell is the
central claim: an expensive model authors a small program that crosses
several semantic decision points without handing control back, keeping
competing explanations alive and gathering discriminating evidence for each.
Run-ahead is then another way to execute the same program, on a trigger
instead of on authoring. Testing the cell first tests the ambition without
making request interception or wake suppression depend on it, and it is
entirely in the model's hands and inspectable in the notebook.

**Second: typed request triage (8), with decision memory (1) as its first
instance.** Eligibility for prefilling is explicit per field, never derived
from a field's type; see [typed-request-triage.md](typed-request-triage.md).
It runs in recommendation-only mode from day one and feeds capability 4.

Every margin and threshold in this document is illustrative. Distribution
concentration is not established accuracy; operational thresholds come from
the replay corpora in [evaluation.md](evaluation.md), not from these pages.

**Highest eventual upside: the semantic execution layer (3) combined with
run-ahead investigation (5).** Together they change what an executor round is.
The model stops being the loop that reads tool output and becomes the author
of bounded microprograms and the consumer of witnesses. That is the shift from
"agent with tools" to "AI-powered software with an agent at the edges", and it
is the one that scales with cheaper judgments rather than with more model
rounds.

**The helper that grows.** Because bindings persist and handback reasons are
journaled, the model that resumes from `NeedsJudgment` can, in the same
session, add the branch it just took by hand to the resident helper. Over a
month the investigation cell's fallback rate per reason is the measure of how
much System 2 has been compiled into System 1, and it is visible in the
notebook rather than in a release.

**Most surprising emergent capability: run-ahead.** Once judgments cost 200 ms
and no model round, Shoal can execute the likely continuation of an expensive
model's next decision before that model wakes: investigate every failing check,
pre-classify every review finding, pre-match every question against decisions,
pre-select the check for every candidate. The model then wakes into a state
where its most probable next three actions have already happened and their
evidence is attached, with distributions showing where the prediction was
uncertain. The frontier model becomes a branch-misprediction handler.

**Fastest falsifications.**

1. **The central experiment.** One real failure with three plausible
   explanations, two evidence-gathering stages, one returned evidence bundle.
   Compare the authored cell against a fixed search recipe and a
   frontier-led investigation on outcome correctness (independently checked,
   not matched against the recorded agent), elapsed time, reads, and frontier
   rounds. If the cell does not reliably do useful work beyond its first
   choice, the rest of this document is a design inventory without a
   mechanism.
2. Label fifty real `WorkChanged` question events with independently checked
   outcomes, not merely what the owner did. If `governing` agrees under 85%
   with the checked outcome or falsely answers over 5%, decision memory is
   dead and capability 4's routing loses its target.
3. Replay fifty candidate lifecycles. If predicted `ContractQuestion`
   precision is under 60%, adaptive review degrades to today's behavior with
   an extra call.
4. Relabel the decision pool keys in test 2 with opaque ids. If agreement
   drops more than ten points, the key-bias finding means pool descriptions
   must carry the full meaning and keys cannot be the semantic handle.
