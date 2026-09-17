# Evaluation: replayable benchmarks and fastest falsifications

Recorded 2026-09-16. Every capability in [shoal-leverage.md](shoal-leverage.md)
must earn its place against recorded Shoal behavior. This document defines
the corpora, the measurements, and the thresholds.

## Principles

- **Replay, not synthesis.** The research crate's frontier and simulation
  runs measured capacity and shape over synthetic worlds. Efficacy is measured
  over what Shoal actually did: journals, `workHistory`, batch request logs,
  and retained notebook cells.
- **Recorded actions are baselines, not ground truth.** Matching the old
  agent could reproduce exactly the wasted work these mechanisms exist to
  remove. Every corpus therefore carries an independently checked outcome:
  did the witness explain the failure, did the delivered decision hold
  through integration, was the finding really contractual. Agreement with
  the recorded action is reported separately as a baseline comparison.
- **Untaken branches are fetched, not scored.** When a replayed judgment
  selects a branch the recorded session never explored, its evidence is
  gathered for real from the recorded revision. A transcript cannot say what
  an unexplored read would have shown.
- **Recommendation-only first.** Each capability runs beside current behavior
  and journals what it would have done. The comparison is against the
  checked outcome first and the recorded action second.
- **Separate the gates.** A wrong judgment, a wrong policy threshold, a
  missing candidate, a stale pool, and a provider failure are different
  failures. Every replay record carries the packet, the response, the policy
  input, and the decision so each can be attributed.
- **Relabel to detect key bias.** Every Choice benchmark reruns with opaque
  keys. A large drop means descriptions do not carry the meaning.

## Corpora

| Corpus | Source | Unit | Label |
|---|---|---|---|
| Question events | `workHistory` `WorkChanged` with opened questions, plus the owner's subsequent action | one question | checked outcome (which decision actually governed, as shown by what integrated); the owner's recorded action as baseline |
| Wake reasons | batch driver journal of `WakeParent` and the parent's next actions | one reason | continued mechanically, delivered decision, batched, decided, halted |
| Candidate lifecycles | `Produced` through `Done` or `Stopped` with every reply | one candidate | reviewer verdicts, finding kinds, `ContractQuestion` or not, repair count |
| Investigations | retained notebook cells and journals for failing checks with a later identified witness | one inquiry | checked witness (the span whose change fixed the failure); the recorded path, rounds, and bytes as baseline |
| Forks | every `taskContext`, `reviewContext`, `designContext` construction and the forked agent's first rounds | one fork | pieces used in the first three rounds, rounds to first substantive action |
| Typed requests | journaled inter-agent typed requests and replies | one request | reply field values |
| Actor snapshots | periodic `workingAndAbnormal` snapshots from runs where duplicate or blocked work was later discovered | one snapshot | which workers were later found duplicated or blocked, and when |

Corpus construction is deterministic Haskell over existing journals. Checked
outcomes come from what later happened in the same journals wherever
possible: the fix that landed, the decision that survived review, the
consumer that broke. Where no later event settles it, a human labels the
outcome, and that label is distinct from the recorded action.

## Per-capability benchmarks

### Decision memory

Corpus: question events, target fifty or more. Measure agreement between
`governing` and the delivered decision, false-answered rate (Jev delivers a
decision the owner did not), merge precision, consult precision, and
specialist sessions avoided. Justified if agreement is at least 85%,
false-answered under 5%, and at least a quarter of questions handled without
a model round.

### Wake economy

Corpus: wake reasons. Measure wakes avoided, decisions delayed past a
labeled deadline, wrong mechanical continuations, and root-cause batching
precision. Justified if wakes drop by a third with zero wrong continuations on
halt-class reasons and no delayed decision past its deadline.

### Semantic execution layer: the central experiment

This runs first. One real failure with three plausible explanations, two
evidence-gathering stages, one returned evidence bundle. Three arms:

- the authored cell, keeping competing explanations alive and gathering one
  discriminating observation per live hypothesis before judging again;
- a fixed search recipe with the same read budget and no judgments;
- a frontier-led investigation as Shoal runs it today.

Measure outcome correctness against the checked witness, elapsed time, reads,
frontier rounds, and whether the returned bundle contained the discriminating
evidence. A cell that returns `NeedsJudgment` is not a failure; measure it on
handoff quality:

- steps usefully completed before handing back;
- whether the returned question is the one the checked outcome turned on;
- whether the resuming model could continue from the resident bindings
  without recomputing anything;
- the handback reason, so recurring reasons can be counted per helper.

Then widen to the investigations corpus, target thirty, with untaken branches
fetched for real. Justified if the cell's correctness plus well-prepared
handbacks is at least the frontier-led arm's correctness at under half the
rounds, and better than the fixed recipe at equal reads. Track the fallback
rate per reason over time: a helper whose handbacks fall as branches are
added is the growth signal this set predicts. If the cell does not do useful
work beyond its first choice, nothing else in this set should proceed to
interception or wake suppression.

### Adaptive review

Corpus: candidate lifecycles, target fifty. Measure `premise_unsettled`
precision and recall against actual `ContractQuestion`, predicted finding kind
against reviewer findings, and first-check selection against the check that
actually failed. Justified if contract prediction precision is at least 70%
and reviewer sessions per landed candidate drop by a quarter with no increase
in integration failures.

### Investigation hylo with run-ahead

Corpus: investigations. Run the hylo from the failing diagnostic with fixed
budgets. Measure witness recall, spans read, wall time, and, in live
recommendation mode, the fraction of wakes where the model's first round
cited the bundle. Justified if recall exceeds 70% and citation exceeds half.

### Context compiler

Corpus: forks. Measure essential-piece drop (a piece the agent used in its
first three rounds that the compiler excluded), rounds to first substantive
action, and context bytes. Justified if essential-drop is under 2% and rounds
to first action fall by one on average.

### Continuous supervision

Corpus: actor snapshots. Measure detection lead time for later-confirmed
duplicates and semantic blockers, and false steering rate. Justified if
duplicates are flagged one wake earlier with false steering under 10%.

### Typed request triage

Corpus: typed requests. Measure field agreement against checked outcomes on
annotated judgment fields, auto-reply rate for closed requests, overwrite
rate in mixed requests, and, because overwrite rate misses confident errors
nobody checks, a sampled audit of unoverwritten prefilled fields against
checked outcomes. Justified if checked agreement exceeds 90%, audit error is
under 3%, and a fifth of requests auto-reply.

## Coverage and savings, the headline numbers

For each helper, report two numbers before anything else:

- **Reliable coverage.** The fraction of invocations resolved without
  handback whose outcome was checked correct. This is the number the user
  cares about; ninety to ninety-five percent with a safe fallback is the
  target that makes the mechanism worth its cost.
- **Model rounds and tokens saved.** For handled invocations, the rounds the
  recorded baseline spent; for handed-back invocations, the difference
  between the baseline's rounds and the resumed model's rounds after the
  prepared handoff. Report input tokens the same way.

The failure to watch is coverage bought with wrong confident answers: an
unchecked "resolved" is worth less than a handback. Only checked outcomes
separate the two.

## Handoff quality, for every capability

Every capability has a handback path to the current model turn. Report, for
each: fraction of invocations that hand back; handback reasons; work
completed before handback; and whether the handback carried the evidence the
model then used. A high handback rate with precise, well-evidenced questions
is a good early result. A low handback rate with wrong confident answers is
the bad one, and only checked outcomes distinguish them.

## Cross-cutting measurements

- **Latency and cost per packet** from `usage` and wall time, so the cadence
  rule can be checked: a cell should not spend more time in Jev than in
  tools.
- **Stability under relabeling** for every Choice benchmark.
- **Stability under repetition**: rerun a sample three times; report
  argmax flips and mass variance. The per-commit experiment flipped a near-tie
  across runs; policy must be designed for that.
- **Provider failures** counted separately and never as judgment errors.

## The four fastest falsifications

1. **The central experiment** above. One failure, three explanations, two
   stages, one bundle, three arms. This tests the capability everything else
   reuses, before interception or wake suppression depend on it.
2. **Decision memory agreement.** Fifty question events with checked
   outcomes. Under 85% agreement or over 5% false-answered kills decision
   memory and removes adaptive review's routing target.
3. **Contract prediction.** Fifty candidate lifecycles. Under 60% precision
   means adaptive review is today's behavior plus a call.
4. **Opaque-key relabeling** of experiment 2. A drop over ten points means
   the key-bias finding forces full meaning into descriptions and keys cannot
   be the semantic handle, which changes the pool design.

Every percentage above is a proposed bar, not a measured property.
Distribution concentration is not accuracy, and no threshold in this set is
operational until the corpus has produced it.

## What would make this a core mechanism

The thesis holds if, across a month of ordinary swarm operation in
recommendation-only mode, the journals show that at least a quarter of
planner and specialist model rounds could have been avoided with no
recommended action that a human labeled as wrong on a halt-class or
authority-bearing decision. That is the bar for flipping any capability from
recommendation to action, and it is measured from the same journals the
capabilities write.
