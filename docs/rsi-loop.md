# The RSI loop

Exomonad is an arena for authored harnesses and orchestration. We develop it by
using it for useful work, comparing the run with an explicit ideal, and building
capabilities that make better outcomes possible. Building `exomonad-harness` is
the current product workload. A wave is a local work cycle, not a release of the
whole vision.

## 1. Orient and choose

The supervisor reads the previous run's artifacts and interviews, verifies
claims against source/history, and reconciles changes already made. Record:

- **Product outcome:** what useful work should be completed, and what proves it?
- **Ideal execution:** how should decisions, information and work flow?
- **Capability frontier:** what useful workflow would we like to express that
  is currently awkward or impossible?

Separate confirmed repairs, hypotheses to test, and exploratory ideas. A missing
example differs from a missing primitive. An existing passing component differs
from a verified product path. Inspect the owning implementation before building
another mechanism.

## 2. Prepare an experiment

Choose a coherent product slice and a small set of distinguishable interventions.
State the hypothesis, its triggering opportunity, observable outcome and a
counterexample. Define the product invariant and consequential failure path before
delegation. Concurrent acceptance tests use explicit ordering barriers.

Repair established defects without pretending each needs an experiment. Prefer
typed contracts and authored programs where they remove a recurring convention.
Larger architectural changes are appropriate when they enable a concrete desired
workflow; the loop does not constrain development to prompt tuning.

Record exact source, workspace and prompt revisions, executable selection and
focused preflight results. A failed preflight is evidence; distinguish a test
assertion from a compiler, launch or environment failure. Preserve the existing
shared services and use the repository's matched build/check entry points.

## 3. Run and observe

The Exomonad root owns assignment, integration and product delivery. The external
supervisor owns the experiment and observation. Use existing traces, retained
outputs, commits and typed results. Interview at meaningful boundaries. Avoid
creating an observer management tree or waking actors just to collect status.

For each opportunity retain actor/request identity, time, exposed source/prompt,
outcome and an artifact reference. Mark held, missed, pending or unknown;
unexercised behavior is not success. Reported interview evidence remains distinct
from observed behavior. Count model relay work and wrong paths alongside latency,
correctness and useful output.

Mid-wave changes are allowed. Record why, the changed revision, and when affected
actors demonstrably incorporate it. A sent message is not incorporation. When
steering changes the experiment, preserve that fact rather than claiming a clean
comparison.

## 4. Explain and expand

Compare actual execution with the ideal. Ask what caused the gap, what alternative
would remove it, and what new workflows that alternative enables. Trace the owner
of a failed mechanism; do not translate every runtime failure into another prompt
rule. Use deterministic code for facts and semantic judgment for meaning.

Report product completion separately from each hypothesis. Different tasks,
incomplete traces and simultaneous changes limit causal claims. Keep surprising
successes and failed experiments: both can expand or correct the capability map.

## 5. Carry forward

Retain useful changes, revise contradicted ones and preserve unresolved questions
with evidence. Promote an authored procedure to a reusable module/tool/hook when
real consumers need it; promote a runtime primitive when the existing owners
cannot express the required contract cleanly. Avoid duplicate schedulers, stores
and policy owners.

The closing handoff names the integrated product revision and checks, hypothesis
verdicts, remaining blockers, newly possible workflows and the next proposed
experiment. Keep the README's purpose stable and iteration details in the active
plan. Do not call the loop complete merely because a model turn ends.

This pattern was extracted during preparation of
[iteration 1](../plans/rsi-iteration-1.md). Its effectiveness remains subject to
the same observation and revision as the orchestration policies it evaluates.
