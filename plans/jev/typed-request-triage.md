# Typed request triage

Recorded 2026-09-16. The capability ranked first for implementation in
[shoal-leverage.md](shoal-leverage.md). Decision memory is its first
instance.

## The idea

Shoal agents talk through typed values. Agent A defines a record, sends a
typed request to agent B asking for one, and B writes Haskell over Tidepool to
construct it. Many of those records are mostly judgment-shaped:

```haskell
data Verdict = Verdict
  { applies   :: Bool            -- does the finding apply to this candidate
  , severity  :: Severity        -- Cosmetic | Degraded | Blocking
  , owner     :: Candidates Actor -- which listed actor should act
  , reasoning :: Text            -- why
  }
```

Three of the four fields are a Noul, a Score, and a Choice over a supplied
pool. Only `reasoning` is generative. Today B wakes, rereads the candidate and
the plan, and fills all four. With Jev, Haskell fills the three from state A
already retains, and B either never wakes or wakes with three fields prefilled
and their distributions attached.

## Derivation

A generic derivation over the request record maps fields to questions:

| Field type | Question | Instruction source |
|---|---|---|
| `Bool` | Noul | the field's documentation |
| closed enum with `Bounded`, `Enum`, unordered | Choice over constructor names | documentation per constructor |
| closed enum marked ordered | Score over constructors in declaration order | documentation per constructor |
| `Candidates a` | Choice over the supplied pool keys plus a no-match key | pool descriptions live in state |
| `Maybe judgment` | the judgment plus a presence Noul | |
| `Text`, code, free structure | not derived; generative | |

The derivation produces wire structure only. A field's type does not
establish that Jev can answer it: a `Bool` might mean "looks relevant" or
"every execution preserves this invariant", and only the first is a judgment
over retained state. Eligibility for prefilling is therefore explicit. The
author of the record marks each judgment field with the question to ask and
the evidence it requires:

```haskell
  , applies :: Judged Bool
      "Does the finding in `finding` apply to `candidate`, judged from the diff and the acceptance text?"
      '[Finding, CandidateDiff, Acceptance]
```

A field without that annotation is generative even if its type is `Bool`. A
request is closed only when every field is annotated and the state the
annotations require is available; otherwise it is mixed or not prefillable.
The annotation is the contract that makes a prefilled value inspectable: the
wake shows the question that was asked and the evidence it was asked over.

The derivation yields the same mode-interpreted schema record the DSL uses
everywhere, so the authored program can inspect, extend, or override the
generated packet with premise-prefixed extras before sending.

## The protocol

1. A sends a typed request to B as today. The request value is retained.
2. Before the request reaches B's mailbox, A's side (or the coordination
   actor that owns the channel) prepares state from what A already retains:
   the request's own context fields, the candidate pool, relevant accepted
   decisions, recent evidence.
3. One packet is sent. Every derivable field gets an answer with a
   distribution.
4. Policy per field: above margin, prefill; below margin, leave empty.
5. Closed request with every field above margin: construct the reply, attach
   the packet as provenance, deliver to A through the existing reply path.
   B never wakes.
6. Otherwise: B wakes with the partial record. Its cell reads the prefilled
   fields and their distributions, writes the generative fields, and may
   overwrite any prefilled field. Overwrites are journaled against the
   distribution they disagreed with.
7. The overwrite rate per record type is one accuracy monitor, and a weak
   one: it misses confident errors nobody checks. A sampled audit of
   unoverwritten prefilled fields against independently checked outcomes is
   the other. Above a configured overwrite or audit-error rate, prefill is
   disabled for that type until an operator looks.

Nothing here creates authority. The reply is A's typed value produced by A's
side from A's retained state; B's mailbox and lifecycle are unchanged.

## Decision memory as the first instance

`DesignQuestion` in `Project.Types` is a typed request from a worker to its
owner. Its judgment-shaped part, once the owner's retained state is the pool,
is:

- Choice `governing`: keys into the accepted-decision pool on the current
  source ancestry, plus `none_governs`. "Which retained decision decides this
  exact operation and failure condition at the question's source?"
- Choice `relationship_to_open`: keys into the open-question pool, plus
  `no_related_question`. "Is this a duplicate of, answered by, distinct from,
  or in conflict with an open question?" Better as one Choice over the pool
  and a second Choice over the four relationship kinds for the selected key,
  premise-prefixed.
- Noul per decision `applies_at_revision`: "Does `d` still apply given its
  source and the question's source, or has the premise changed?"
- Noul `contradicts_accepted`.
- Noul `blocks_obligation`.
- Score `answer_kind`: local evidence suffices / needs a specialist reading /
  needs a project decision.
- Premise-prefixed for the top candidates: "If `d` governs, does `d.summary`
  need amendment to cover this finding?"

Policy in the work actor's sink:

| Judgment | Action | Wakes anyone |
|---|---|---|
| `governing` = `d` with margin, `applies_at_revision` high, no contradiction | `withDecision d`, deliver `decisionContext` through `updateDecision` | no |
| duplicate of open question | `raiseQuestion` merges; note attached to the open one | no |
| distinct, `blocks_obligation` high, `answer_kind` = project decision | `consultDesign` directly | specialist only |
| distinct, not blocking | queue for the next ordinary wake | no |
| `contradicts_accepted` above threshold, or near-tie on `governing` | wake owner with the packet attached | owner |

Everything delivered is evidence, not authority. The worker's next progress
still flows through the sink, and every delivery is journaled with its
distribution so a wrong answer is visible and reversible.

## Why this early, and why not first

The authored investigation cell comes first; see
[shoal-leverage.md](shoal-leverage.md). It tests the central capability, a
program crossing several semantic decision points without returning control,
without making request interception or wake suppression depend on it. Typed
request triage follows because:

- The candidate pool already exists as typed records: `AcceptedDecision`,
  `Question`, `DesignQuestion`.
- The delivery path already exists: `updateDecision`, `withDecision`,
  `raiseQuestion`, `consultDesign`.
- The hook point already exists: the `WorkSink` in `Project.Routing` sees
  every `WorkChanged` with opened questions.
- It runs in recommendation-only mode from day one: journal the packet and
  the would-be action beside today's behavior, compare later.
- The evaluation corpus exists in `workHistory`: every question event and
  what the owner did.
- It feeds adaptive review, which needs `premise_unsettled` to route
  somewhere.

## Where it goes next

Once decision memory holds, the generic derivation covers every typed request
whose record has judgment-shaped fields: review verdicts, repair-owner
selection, incorporation acceptance, wake-versus-queue decisions, and the
recipient selection in swarm traffic. Each is the same mechanism with a
different record. The library grows by authors documenting fields as
questions, not by adding Jev-specific code paths.

## Risks specific to this capability

- **Silent wrong answers.** A prefilled field that is wrong and never
  overwritten is the failure mode. Mitigations: margins per field type,
  provenance on every prefilled value, overwrite-rate monitoring, and
  recommendation-only mode until the replay corpus shows agreement.
- **Pool staleness.** A decision pool built from a stale ancestry answers
  from superseded decisions. `applies_at_revision` helps but the pool
  construction is deterministic and must use the question's source revision.
- **Key bias in pools.** Decision keys are model-facing. Pool descriptions
  must carry the full meaning; the falsification experiment in
  [evaluation.md](evaluation.md) checks whether opaque keys degrade agreement.
- **Instruction quality.** An annotation written as a description ("the
  severity") rather than a question ("how severe is the finding, judged by
  impact on the obligation") yields worse judgments. The derivation rejects
  judgment fields without a question and an evidence list.
- **Type-shaped overreach.** The temptation to prefill every `Bool` is the
  failure Astra named: a type says how to encode an answer, not whether the
  question is answerable from retained state. Eligibility stays explicit.
