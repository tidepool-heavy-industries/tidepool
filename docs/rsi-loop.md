# The RSI loop

Exomonad is an arena for authored harnesses and orchestration. We develop it by
using it for useful work, comparing the run with an explicit ideal, and building
capabilities that make better outcomes possible. Building `exomonad-harness` is
the current product workload. A wave is a local work cycle, not a release of the
whole vision.

## Working defaults

Prioritize parallel work and Luna delegation, with Sol owning shared decisions
and integration. Prefer broad worker trees, add depth for coherent component
ownership, and keep a modest experimental bias toward useful nesting. Independent
review within the tree provides assurance; a busy root or a repair round is not
itself a failure. Measure overlapping useful work, work shifted to Lunas, defects
caught by independent review, and avoidable coordination separately.

Include one bounded ambitious orchestration experiment per iteration. Explore
Haskell notebook composition deliberately: combine commands, retained evidence,
Jev judgments, child admission, routing and joins where that reduces total tool
calls and model rounds while preserving failure boundaries and evidence. Promote
successful compositions into compiled examples. Code owns authoritative facts;
Jev supplies typed semantic judgments where those facts need interpretation.

Aim for near-zero avoidable orientation at actor activation. Supply stable
interaction mechanics in the shared prompt, role-specific workflow in role
instructions, and current source/environment/ownership/check facts in the task
packet. The first useful action should be apparent without reconstructing how
to use the harness. Validate environment and helper availability before promising
them; prompts do not repair a broken runtime surface.

Audit each actor's initial tool sequence through its first useful task action.
Record time and calls spent on harness/API/environment discovery separately from
necessary source inspection, contract analysis and failure investigation. A first
edit is not the only useful action. Count rediscovery and wrong turns throughout
the assignment too. Replace repeated discovery with precise, source-backed entry
points and compiled examples; avoid blanket bans on reading or a larger ritual
inventory. Keep changing facts after the shared prefix so inheritance stays useful.

A clean win is a known, implementable solution, even when implementation spans
several layers. Separate that work from unresolved design questions and experiments.
For the current harness workload, prove extension points through standalone stubs
and production consumers before attempting Exomonad integration.

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

## Threshold audits for every run

Choose explicit operation/threshold pairs before the run and refine them when
new friction appears. Audit every observed crossing through a stated cutoff,
not just the worst anecdote. Begin with these triggers and tune them from
measured evidence; they are investigation triggers, not automatic failures:

| Axis | Initial trigger | Investigate |
|---|---|---|
| Latency | Every Haskell cell over 10 seconds | Queue, compiler phases, checkout wait/hold, execution, judgment, unaccounted time |
| Repeated work | Repeated identical lookup/check/compile with unchanged inputs | Missing supplied context, duplicate owner, cache validity, replay |
| Orientation | Any discovery of facts the host or requester already knows | Prompt/API gaps, environment setup, source/contract packets |
| Coordination | Repeated unchanged poll, duplicate review, missed question, or lost update | Event routing, pending vs terminal state, request ownership |
| Correctness | Every rejected typed reply, zero-test pass claim, or wrong-source review | API shape, source identity, actual executed evidence |
| Resources | Unexpected bulk copy, retained resource after cleanup, or budget crossing | Source/mount boundaries, lifetime ownership, cancellation, storage growth |
| Context and output | Repeated truncation, output rereads, or rediscovery after a fork | Evidence selection, retained handles, shared prefix and inheritance |
| Model effort | Repeated root relay or judgment on code-decidable facts | Typed actors, reusable compositions, deterministic guards |

Thresholds can be events as well as numbers. Audit every type/parser error,
explicit expression of confusion, misunderstood contract, wrong API assumption,
failed tool invocation and recovery loop. Treat confusion inferred from behavior
as a hypothesis until the trace or interview supports it. A rejected cell is
observable; why the actor wrote it still needs investigation.

Give investigation subagents bounded episodes or clusters of the same symptom.
Each receives the exact call/error, relevant preceding context, exposed prompt/API
revision and eventual recovery. Ask what the actor knew, what it reasonably
inferred, what was missing or misleading, and the smallest change that would
prevent recurrence. Distinguish expected exploratory feedback from avoidable
misdirection; count attempts, recovery work and downstream effects separately.

Possible owners include the shared prompt, assignment renderer, Haskell API,
compiler diagnostic, environment provisioning and runtime. Prefer a better type,
default or diagnostic when repeated prompting would make callers maintain an
invariant. Preserve unfamiliar but valid use cases: do not turn each error into
a new prohibition. Interview the affected actor where intent remains unclear,
and review proposed fixes against both the failed case and ordinary valid use.

For each crossing record operation/actor identity, input/source revision, threshold,
observed value, phase breakdown, outcome and evidence. Include failed, cancelled
and still-running operations; completed-call timing rows alone miss hangs. State
coverage and missing instrumentation. Preserve units and distinguish wall time,
CPU, allocations, retained bytes, tool calls and model rounds. Nested timing spans
must not be added as independent costs.

Group crossings by owning mechanism, then use bounded parallel investigations.
Classify findings as necessary task work, avoidable repeated work, contention,
implementation defect, or unresolved. Seek deletion of redundant work and cleaner
ownership before larger caches, more workers or instructions to avoid the API.
Report measured facts separately from causal hypotheses; contention coinciding
with a slow phase does not establish how much time it caused.

Each actionable finding becomes an owned fix or explicit deferred card, with a
failure case and a check that exercises the real consumer. Compare before/after
on the same workload where possible, then observe the next run for regressions
and shifted costs. Retain compact tables/diagrams and link detailed evidence;
use existing traces before adding instrumentation. An audit is complete when
crossings are accounted for and actions assigned, not when every cost is zero.

## 3. Run and observe

The Exomonad root owns assignment, integration and product delivery. The external
supervisor owns the experiment and observation. Operator and developer experience
is part of the same experiment: retain build, launch, monitoring, recovery and
cleanup friction encountered by the supervisor, with the same evidence standards
as actor friction. Track preparation and supervision costs separately from the
product run; either may motivate the next improvement. Use existing traces, retained
outputs, commits and typed results. Interview at meaningful boundaries. Avoid
creating an observer management tree or waking actors just to collect status.

While a wave runs, use bounded subagent work to remove confirmed technical debt
and improve the next development cycle: duplicate mechanisms, unclear ownership,
dead paths and types that leave invariants to callers. Give each change an owner,
real consumer and focused verification; keep its integration separate from the
active wave and its executable stable. Return findings to the same RSI backlog.

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
