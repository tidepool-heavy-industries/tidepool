# Jev pattern building blocks

Status: design sketches for discussion and prototyping. Names and snippets below
are proposed interfaces, not compiled or shipped APIs. Keep them out of model
prompts until the corresponding examples execute. Inanna endorses opinionated,
composable patterns, with defaults reevaluated between runs.

## What an author should be able to do

Write a small decision tree around a real task, then hand it to an existing actor
or call it from a notebook. Deterministic Haskell handles known facts and effects;
Jev resolves the small semantic branches. A useful first result replaces one
boring multi-call sequence. It need not automate the whole assignment.

There are three authoring levels:

1. Existing Jev questions, packets, alternatives and confidence policies.
2. Reusable judgment patterns with named inputs and good default criteria.
3. Project procedures that compose those patterns with commands, requests and
   record actors. These procedures retain their context and can pass to children.

A pattern constructs a question or packet plus its interpretation; constructing
it does not issue inference. Independent questions sharing evidence can join one
packet. Haskell branches before requesting evidence that depends on an answer.
Authors can replace a criterion, evidence projection or interpretation policy
without rewriting the workflow or its transport.

## 1. Gate a prepared action

**Use:** decide whether an already defined action fits the observed situation.
Examples: suggest an existing helper, ask for a missing consumer at an agreed
checkpoint, or take a bounded recovery step.

Inputs: named applicability criteria, exclusions, an evidence projection, and
an uncertainty policy. A named recipe supplies these defaults; callers can
replace individual pieces with ordinary functions or record updates.

Output: applicable, inapplicable, or unresolved, retaining the original judgment.
Unavailable service and absent required evidence remain distinguishable.

Default decision tree:

```text
required evidence available?
  no  -> request the named missing evidence or return unresolved
  yes -> deterministic exclusion applies?
           yes -> skip
           no  -> semantic applicability
                    applies     -> return the prepared action as a value
                    inapplicable -> skip
                    unresolved   -> return to the caller's uncertainty branch
```

The generic piece does not execute that action. The caller chooses its effectful
continuation. A reusable `consumerCheckpoint` recipe would know what counts as a
production consumer, but the project's agreed checkpoint and evidence remain
inputs. Silence alone cannot establish a missed milestone.

**First evaluation:** missing main entry point, acknowledged producer dependency,
no agreed milestone, and already-incorporated consumer. Count useful interventions
and unnecessary ones, including deterministic paths that made no Jev call.

## 2. Route within a contract

**Use:** handle routine review findings without asking the owner to relay each
message. The existing ReviewFlow remains the lifecycle owner.

Inputs: original exact candidate/review receipt, the assigned contract, offered
routes, and escalation criteria. Default routes are continue, repair within the
contract, and ask the parent. A project can offer a different typed route set.

Decision tree:

```text
receipt names the expected source and review basis?
  no  -> source/evidence refusal
  yes -> deterministic acceptance or repair budget already settles next step?
           yes -> take that step
           no  -> judge scope and need for a shared decision
                    routine and within scope -> retained implementer repair
                    shared decision required -> parent with evidence packet
                    unclear                  -> parent with unresolved judgment
```

Scope and shared-decision questions can share a packet when both are useful and
independent. A service failure never becomes permission to repair. An ambiguous
judgment need not produce a fabricated explanation: retain the criteria, answer,
confidence/policy evidence and original finding for the parent.

**Replaceable pieces:** contract projection, routes and their criteria, named
confidence policy, parent-packet projection, repair continuation. The recipe
keeps useful defaults so ordinary use requires only the contract and handles.

**First consumer:** existing checked review continuation. Measure owner relay
turns, repair correctness, unnecessary escalation and invalid source refusals.

## 3. Compare an update with handled evidence

**Use:** propose whether a coordination update warrants owner attention.
Compare incoming facts against explicit handled/incorporated evidence, not a
window of recent transcript text.

Inputs: source-identified update, relevant handled facts, current obligation,
and a definition of a meaningful change. Named defaults favor preserving novel
questions, failures, conflicts and unincorporated useful results.

Decision tree:

```text
exact event identity already processed?
  yes -> reuse the retained result; no new judgment
  no  -> structural new failure/question or missing comparison context?
           definite urgent event -> attention under declared policy
           missing context       -> unresolved
           otherwise             -> semantic comparison
                                    meaningful change -> attention
                                    all repetition    -> record
                                    mixed/uncertain   -> attention or unresolved
```

A repeated commit can carry a new failing check or question. Compare those facts
as well as the OID. Acknowledgment does not establish incorporation. Deterministic
shortcuts must be explicitly valid for this workflow, not keyword heuristics.

**First consumer:** shadow adapter on the existing Work collector. Real delivery
continues unchanged; compare proposed decisions against what the owner actually
needed. Suppression requires a later decision and evidence. Expose packet refusals
and uncertainty so a cheap-looking trial cannot hide missed work.

## 4. Gather once, then decide

**Use:** replace a repetitive investigation setup: inspect a failed command,
recover the right output, find the relevant source, and hand over a compact packet.

Inputs: original command/result reference, a small set of available evidence
sources, a retrieval budget and a question that the packet must answer.

Decision tree:

```text
original evidence sufficient by a known structural rule?
  yes -> construct the packet directly
  no  -> select the useful missing evidence from available sources
           known source -> bounded deterministic retrieval
           uncertain    -> return the gap and existing evidence
        assess the expanded packet once
           sufficient -> return selected evidence with full references
           still missing -> parent; no unbounded investigation loop
```

The retrieval bound counts actual operations; it is not a recursive 'research
until confident' instruction. Reuse retained output instead of rerunning commands.
A selector can return a typed source or prepared read action; Rust continues to
own subprocesses and resources. Existing investigation/evidence helpers supply
the implementation seams.

**First evaluation:** truncated output, missing receipt, useful source span, and
unavailable evidence. Measure retrievals, packet size, model follow-up turns and
whether selection omitted the decisive fact.

## 5. Rank a bounded set, then apply an independent gate

**Use:** choose among available evidence snippets, reusable helpers, or safe next
steps when a single brittle winner choice is unhelpful.

Inputs: supplied candidates, a suitability rubric, and an applicability gate.
Keep candidate identity and payload together through scoring and selection.

Score the candidates in one per-item packet. If the gate can also be evaluated
from that same evidence, ask it speculatively in that packet and consume only the
chosen candidate's gate. Otherwise fetch the chosen evidence before the next call.
Use the next ranked candidate only under an explicit bounded fallback policy.

This is useful when preferences trade off; it must not average away a hard
exclusion. No suitable candidate is an ordinary result. Do not add this pattern
where deterministic filtering plus one small choice is sufficient.

**First evaluation:** compare useful selection against its extra question/token
cost. Keep this behind the first two concrete consumers; it is a candidate for
reuse, not an obligation to make every router rank alternatives.

## Composition: one project decision tree

A review workflow can combine the pieces without adding a workflow engine:

```text
original failed-check receipt
  -> deterministic exact-source and execution checks
  -> gather missing diagnostics, if needed and within budget
  -> one scope/escalation packet
  -> branch on typed outcome
       repair -> existing repair actor -> new exact candidate -> existing checks
       ask    -> parent receives retained evidence and specific unresolved choice
       stop   -> retained infrastructure/evidence failure
```

Ordinary functions define projections and policies. Existing packets combine
independent questions. Existing record actors own state, deduplication and event
subscriptions. Existing effectful continuations execute the selected branch.
There is no requirement to encode the tree in a second AST or a universal monad.
Only introduce additional composition machinery if these real call sites expose
repeated structure that ordinary Haskell does not express cleanly.

## Evidence and cost contracts

- Retain the original evidence identities, question/policy version, actual
  answer and selected continuation. Keep usage/diagnostics through the existing
  response/host trace; avoid building another durable log or cache.
- Preserve the semantic answer separately from confidence interpretation. A
  different policy can interpret the same retained answer without another call.
- Reuse only when evidence and question meaning are unchanged. Changed evidence
  gets a fresh episode; repeated delivery alone does not justify more inference.
- Use relevant structured projections with explicit budgets. Refused or omitted
  evidence is visible; the pattern must not truncate away a necessary fact and
  call the remainder sufficient.
- Model confidence is evidence for a decision, not runtime authority. Jev does
  not manufacture a review receipt, source observation or successful check.
- Keep independent packet composition and dependent calls visible enough that
  authors can predict whether their tree makes zero, one or two model requests.

## First implementation and review sequence

1. Write author-facing examples for contract routing and update comparison.
2. Factor only their shared question/interpretation structure over the pinned
   Jev API. Keep project-specific defaults in named recipes.
3. Compile and run the exact examples; test deterministic routing and uncertainty
   separately from live semantic cases. Inspect synthetic Jev cases personally.
4. Plug them into existing review and shadow-notification actors. Preserve their
   original handles, cleanup outcomes and one owning state machine.
5. Advertise the compiled recipes at their discovery points, including one
   customization example and when another approach fits better.
6. Between runs review decisions, missed opportunities, overrides, packet cost and
   saved frontier turns. Tune the defaults; prune repeated failures. Usage count
   alone is neither success nor failure.

Applicability and evidence gathering follow as consumers demand them. Ranking is
an exploration candidate. None of these designs claims measured savings yet.

## Implemented exemplar pass

Three patterns now have six compiled clients: command triage, typed evidence
selection and update comparison. The operator exercised exact prepared requests
against live Jev and replayed the replies through the typed interpreters. Using
the examples led to shared evidence state, replaceable complete candidate
renderers, domain-specific failure wording and retained policy types.
See [the trial report](../docs/reports/wave18-audit/jev-pattern-trials.md) for exact
revisions, cases, costs, uncertain outcomes and integration limits. The remaining
catalog entries above are still designs, not implemented APIs.
