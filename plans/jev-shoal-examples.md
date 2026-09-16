# Jev in Shoal: practical semantic decisions

Design exploration, 2026-09-16. Read alongside [the DSL design](jev-dsl.md)
and [observed API contract](../jev-integration/CONTRACT.md). These are proposed
applications, not implemented features or measured task-quality results. Haskell
below is illustrative DSL notation; only the existing `PlanF`/`hyloM` signatures
are established API. Every provider call uses the single proposed `jev` operation.

Follow-on: [14 synthetic live probes](../jev-integration/SHOAL-EXPERIMENTS.md)
now exercise these judgments with contrasting cases. All matched their initial
qualitative expectations on one run; composed workflows remain untested.

The useful unit is a semantic judgment over a bounded observation and concrete
alternatives. Haskell keeps handles, source locations, receipts, and continuations;
Jev sees structured descriptions. The result selects or annotates existing data.
This permits useful helpers that never generate a sentence of their own.

## 1. A work coordinator that recognizes consequential updates

**First integration candidate.** `Project.Routing` already retains work history,
progress deltas, unanswered questions, candidate commits, and settlement results.
Its `WorkSink` is an effectful project-policy seam. Currently `workMessage` selects
notifications using event structure; a semantic sink could handle ambiguous events.

Example: an implementer reports “the new index preserves the old lookup contract,
but callers retaining offsets across edits must switch to stable IDs.” A reviewer
needs this immediately if reviewing such a caller; an unrelated implementer can
read it later. The word “blocker” need not appear anywhere.

Supply the exact update, candidate revision, active assignments, accepted decisions,
and known dependencies. Each candidate agent's structured description includes
its responsibility, current question, relevant paths, and the work it is waiting
on. Keep its actual `AgentRef` as a local payload.

Ask who should handle the update, with an explicit unassigned outcome. Then ask
about urgency for the selected recipient using that recipient's current task.
These are dependent calls; a shared urgency answer must not silently pretend to
be conditional on the first question's answer in the same request.

If several recipients legitimately need it, use per-recipient Noul judgments for
relevance, rather than treating a Choice distribution as independent membership.

```haskell
data Attention mode = Attention
  { changesNextAction :: mode :- Noul
  , contradictsAssumption :: mode :- Noul
  , delayCost :: mode :- Score DelayLevels
  }
```

Use concrete delay levels: background information; useful at the next checkpoint;
blocks the next step; continuing now risks invalidating other work. Each level's
structured definition stands alone. Haskell translates the judgments into project
policy, with known failures/deadlines handled directly and ambiguous high-impact
updates surfaced to the coordinator. Do not install untested numerical thresholds.

Retain every event before judging it. Deferring notification must not erase the
update or resolve an outstanding question. Coalesce notifications by exact event
identity and keep existing notification receipts and incorporation distinctions.

**Existing boundary:** `sendMessage` admits steering and returns admission evidence;
it does not expose a wake-versus-next-wake parameter. A first implementation can
retain deferred events in the existing work actor and let the agent read them on
its next activation. That is not delivery into the agent's inbox. True enqueue-only
delivery, if wanted, belongs to the existing Rust mailbox owner. `WorkEffects` is
currently fixed through `CoordinationEffects`; the selected profile must admit Jev
before a sink can call it. A long call also delays that serialized actor's handler;
measure this before choosing to evaluate every progress event there.

**Evaluate:** replay progress traces; label who actually needed each event and
when. Measure missed urgent updates, unnecessary agent activations, and delivery
delay. Begin with recommendations recorded alongside existing behavior.

## 2. A code investigation hylo that returns evidence

**Best DSL stress test.** Give a helper a question such as “where can cancellation
stop a result from reaching its requester?” and a starting symbol. Deterministic
tools enumerate definitions, callers, references, and source spans. At each node,
Jev chooses which relationship warrants inspection for this particular question.

An edge description is a record: source and destination signatures, relation kind,
nearby source, module responsibility, and already observed facts. A useful branch
can therefore win because of what lies behind it, rather than a suggestive name.
The selected payload is an actual `Edge`, with no generated path to parse.

```haskell
data Expansion mode = Expansion
  { follow :: mode :- Option Edge EdgeDescription
  , retain :: mode :- Option Evidence EvidenceDescription
  , unresolved :: mode :- Option InvestigationGap GapDescription
  }

-- Homogeneous dynamic edge collections supply the multiple Follow candidates;
-- this record illustrates the heterogeneous branch payloads.

inspectCoalg :: Seed -> Eff effects (PlanF Observation Seed)
inspectCoalg seed = do
  neighborhood <- observeNeighborhood seed
  judged <- jev (expansionRequest seed neighborhood)
  pure (expansionLayer seed neighborhood judged)

evidenceAlg :: PlanF Observation EvidenceBundle -> Eff effects EvidenceBundle
evidenceAlg layer = do
  judged <- jev (evidenceRequest layer)
  pure (assembleEvidence layer judged)

investigate = hyloM evidenceAlg inspectCoalg
```

The coalgebra's request concerns the next useful seeds. The algebra's distinct
request concerns whether the local source and child evidence support the inquiry,
contradict it, or leave a named gap. The fold returns selected source spans, exact
revisions, traversal paths, and outstanding gaps. A larger model can explain those
facts later. Jev does not generate a proof or prose summary.

Code owns depth, node and call budgets, source snapshot consistency, candidate
enumeration, and cycle handling. A graph must be unfolded into bounded paths or
use traversal-owned visited state; recursive cycles do not disappear because the
driver is a hylo. With concurrent siblings, shared deduplication needs an explicit
owner. Empty neighborhoods and exhausted budgets produce explicit leaf observations.

For one next branch use Choice with stop/unknown outcomes. For several independently
useful branches use per-edge relevance judgments and a deterministic fanout bound.
Ranking edges by Choice probability is a possible search heuristic, but its values
are relative to the supplied alternatives, not independent relevance probabilities.
Choice has a tested maximum of 255 alternatives, including terminal alternatives;
large neighborhoods need deterministic narrowing or explicit hierarchical search.

**Hylo limit:** the algebra cannot return new recursive seeds into this same
traversal. If it discovers a missing witness, return `NeedsMoreEvidence` containing
follow-up seeds. A bounded outer investigation loop can start the next traversal.

**Existing boundary:** `Tidepool.Swarm` already owns `PlanF`, `hyloM`, and effectful
algebra/coalgebra middleware. No LSP effect was found in the inspected Haskell library;
start with search plus parsed source results, or use a separately established LSP
adapter. Do not claim the illustrative `observeNeighborhood` already exists.

**Evaluate:** known investigations with expected source witnesses; measure witness
recall, source reads, end-to-end time, and unresolved reports. Compare with bounded
breadth-first traversal and an ordinary agent investigation.

## 3. An evidence lens for noisy helper results

**Smallest independently useful helper.** A notebook asks “show me the diagnostic
that explains this failed check,” “find the hunk implementing this obligation,” or
“which retained reply answers this design question?”

Deterministic parsing produces real candidates: diagnostic groups, hunks, source
spans, or typed response handles. Jev selects among structured descriptions plus
`NoMatch`/`NeedMoreContext`. Haskell returns the exact selected value, which the
caller can inspect, pass to a command, or include in an assignment.

```haskell
-- Proposed application helper: internally builds a request and calls jev.
selectEvidence
  :: Member Jev effects
  => Inquiry -> Candidates SourceSpan
  -> Eff effects (Either JevError (EvidenceSelection SourceSpan))
```

For a long compiler cascade, first group diagnostics by compiler-provided identity
and locations. Ask which group most directly explains the failure; return the
whole group including notes. Do not ask Jev to reconstruct the diagnostic. For a
large log, preserve candidate coverage explicitly: failure to find a match in a
truncated candidate set is not evidence that the full log has none.

This helper exercises dynamic checked collections, no-match outcomes, candidate
identity, and payloads without JSON instances. Its return should retain alternate
weights so a caller can inspect two plausible candidates instead of hiding doubt.

**Evaluate:** hand-labeled logs and source inquiries; exact-span correctness,
no-match behavior, relevant context retained, and tokens removed from agent context.
This is the best first live semantic experiment because errors are easy to inspect.

## 4. Match new questions to existing decisions without losing the distinction

Two workers ask “who owns retries?” and “does the transport or session rerun a
timed-out request?” Exact keys will not necessarily show these are related.
Conversely, similar wording may refer to different layers or source revisions.

Retrieve candidate open questions and accepted decisions using project/task/revision
metadata. First choose a candidate or no match. In a dependent request, classify
the relationship as duplicate question, answered by this decision, related but
distinct, or conflicting assumptions. Supply actual decision scope and evidence.

Haskell attaches an explicit relationship between the retained records. A duplicate
can share attention; an answer can be suggested to the requester; a conflict can
notify the relevant owner with both original statements. The model does not mark
the question resolved or invent an accepted decision. Revision changes must be
checked before reusing an answer, and semantic applicability still needs evidence.

**Existing boundary:** `Project.Types` has `DesignQuestion`, `AcceptedDecision`, and
revision-bearing evidence; `Project.Routing` retains question changes. Keep their
exact identity logic. Semantic relationships supplement those identities.

**Evaluate:** pairs of genuine duplicates and deceptively similar distinct issues;
false merges are more damaging than missed deduplication. Retain provenance and
make inferred relationships inspectable and reversible.

## 5. Route a failed check to the next useful investigation

A check fails after a candidate lands in a worktree. Exit status, test identity,
revision, and diagnostic parsing are deterministic. The semantic question is which
available next step is most likely to resolve the uncertainty: inspect an observed
contract mismatch, inspect setup failure, hand the finding to the retained
implementer, or ask the owner about a conflicting requirement.

Supply the failure evidence and a structured description for every legal action.
The choices retain different payload types: `DiagnosticGroup`, `CommandPlan`,
`RepairTask`, or `DesignQuestion`. An exhaustive handler record dispatches the
selected branch, using existing command and actor operations.

```haskell
matchChoice answer NextStep
  { inspectDiagnostic = inspectGroup
  , runFocusedCheck = executeApprovedPlan
  , requestRepair = askRetainedImplementer
  , askOwner = surfaceDesignQuestion
  }
```

These are illustrative handlers, not extra Jev effects. Code constructs eligible
actions from the current state: no rerun after budget exhaustion, no request to a
known unavailable actor, no fabricated shell command. Preserve the existing
`RepairOwner` distinction: sending repair to an owner blocked on that delivery can
deadlock. A “flaky” judgment cannot turn a failing check into a passing one.

**Evaluate:** historical failures with known causes. Measure useful first action,
time to obtain the decisive observation, redundant reruns, and escalation quality.

## 6. Choose the next experiment by which hypotheses it separates

This is a richer helper for an investigation actor. Keep a finite hypothesis set,
each with cited evidence, and a menu of existing diagnostic plans. Ask Jev whether
each plan's possible observations would distinguish the leading hypotheses, using
structured descriptions of what the plan actually measures.

Code knows execution cost, permitted resources, and expected output type; Jev judges
semantic discrimination. Code selects an affordable useful plan, runs it, and
builds the next request from the real observation. A frozen environment is part of
that observation. Uncertainty or no discriminating candidate returns control to
the reasoning agent to formulate a better hypothesis or experiment.

Do not call a rubric score “information gain” or do Bayesian updates from unrelated
Choice distributions. Start with a concrete rubric: cannot distinguish these
hypotheses; distinguishes a secondary difference; directly tests the disputed fact.
Retain the full distribution. This helper should follow the evidence lens, once
we have trustworthy candidate descriptions and realistic failures to evaluate.

## What these examples require of the DSL

The actor examples need native question records and structured domain records.
The evidence helper needs homogeneous runtime alternatives, while the diagnostic
dispatcher needs heterogeneous local payloads and exhaustive elimination. The hylo
needs different request schemas at expansion and folding, typed failure values,
and results that can leave a call site without forcing the whole program into CPS.
Nested questions and keyed per-item judgments should preserve their answer shape.

All use one complete request operation. Description building, threshold policies,
candidate narrowing, handler dispatch, and recursion are ordinary Haskell. The
provider bridge validates answers against the precise submitted candidates; labels
never become unchecked paths, actor identities, or commands.

The prior 63 probes established wire behavior, not these applications' accuracy.
Initial successful calls took 153–218 ms; twenty comparable dependent calls alone
would be roughly 3–4.4 seconds before tool work. That arithmetic is not a latency
promise. Count calls and input size, and compare whole-workflow performance.

Recommended sequence: evaluate the evidence lens on fixed fixtures; add an
observational work-notification policy; then exercise both halves of a bounded
investigation hylo. This covers the interesting DSL constraints while producing
useful helpers at each stage.

## Source anchors

- [Work event retention and notification policy](../examples/shoal-workspace/.shoal/Project/Routing.hs)
- [Project tasks, decisions, questions, and repair ownership](../examples/shoal-workspace/.shoal/Project/Types.hs)
- [Record actor definitions and event subscriptions](../haskell/lib/Tidepool/Actor/Record.hs)
- [Notification admission API](../haskell/actors/Tidepool/Actors/Internal/Agent.hs)
- [Hylo and policy middleware](../haskell/lib/Tidepool/Swarm.hs)
- [GHC-only DSL feasibility sketch](../jev-integration/haskell/Sketch.hs)
- TypeSafe [structured descriptions](https://docs.typesafe.ai/primitives/advanced),
  [source-span selection](https://docs.typesafe.ai/cookbooks/semantic_find), and
  [hierarchical traversal](https://docs.typesafe.ai/cookbooks/hierarchical_classification)
  informed candidate descriptions and traversal design; they do not establish
  quality on Shoal tasks. Live pages read during this exploration.

The installed TypeSafe skill guided narrow evidence-based questions, explicit
candidate coverage, and separation of judgments from deterministic policy.
