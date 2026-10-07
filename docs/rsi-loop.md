# The RSI loop

Exomonad is an arena for authored harnesses and orchestration. We develop it by
using it for useful work, comparing the run with an explicit ideal, and building
capabilities that make better outcomes possible. Building `exomonad-harness` is
the current product workload. A wave is a local work cycle, not a release of the
whole vision.

## Outcome and evidence

Begin with the product decision this iteration must support and the artifact
that makes it inspectable: an integrated revision with executed checks, a
replayable failure, or a comparison that distinguishes proposed workflows. Keep
exploration broader than the handoff when useful. Separate product acceptance,
the experiment's hypothesis, and the quality of its measurement; failure to
observe a mechanism is not evidence that the mechanism works or never occurs.

Use value of information to order work. Prefer a source inspection, trace replay
or bounded intervention that could reverse an expensive design choice. Invest
in reusable analyzers and replay drivers when repeated evaluation dominates;
improve their inputs and observations as the investigation develops. Reallocate
independent investigations when the critical path or uncertainty changes. Explicit
iteration commitments still apply, but a confirmed local defect need not acquire
an elaborate experiment before repair.

## Learning from mistakes

Apply kaizen to development as well as the product. When a mistake is found,
retain a short account of the violated expectation, the causal mechanism and
why the existing checks or reasoning allowed it through. Scale this to the
finding: a wrong import needs a compile check, not a new review bureaucracy.
An unresolved cause remains a hypothesis, not a lesson stated as fact.

Distinguish a product defect, a defective test or procedure, and a confirmed
regression. A newly failing test does not establish a regression: compare the
same semantic workload before and after the suspected change. A repaired test
may expose an older defect. Keep fixture corrections distinguishable from
production repairs, and do not attribute failures to nearby performance work
without causal evidence. Source review, compilation and behavioral execution
establish different claims.

Close both loops: repair the behavior and improve the mechanism that should
have prevented or detected it. Follow the lost invariant across issuance,
transport, consumption, replay and cleanup; inspect analogous paths such as
success and abort. Prefer one owning implementation or a type that carries the
known fact. Then exercise the real boundary where that fact could be lost and
check that the new safeguard detects or prevents the original failure.
A mock supplied with the correct answer cannot establish that production
constructs it correctly.

Put the durable lesson where it changes future work: an invariant in the owner,
a regression or generator in the test suite, automation in the existing tool,
or a concise decision rule in the nearest guide. Link retained evidence from
the delivery record; keep incident chronology out of standing instructions.
Name any remaining prevention or detection gap and its owner. If an existing
rule already covers the mistake, investigate why it was ineffective instead of
adding a duplicate. Recurrence is evidence to revisit the mechanism, including
whether the guidance is actionable, discoverable and supported by tools.

## The harness destination

The product workload builds a custom home for Exomonad agents, ultimately replacing
Codex as their harness. Target GPT-6 capabilities directly, with asynchronous tool
calls as an early foundation; earlier-model compatibility is not a requirement.
Use the available Codex source and actual provider contract to ground decisions.
Concurrent execution, model continuation, result delivery and terminality are
separate behaviors to verify.

Typed mailboxes, event hooks and Haskell continuations should be native parts of
that interaction model. Discover where they let authored machinery advance useful
work without another model turn, and where semantic judgment is still valuable.
Prove extension seams with standalone deterministic consumers before integration.
Each wave should produce usable behavior and evidence for the next design slice.

## Evidence sufficiency for semantic automation

For every new or changed Jev judgment, review the exact packet and its production
consumer together. Write down the question, the facts needed to answer it, where
those facts came from, and what happens when they are missing. Commit IDs, job
handles, check names and summaries locate evidence; they do not supply the source,
diagnostics or acceptance evidence the judgment may need. Retrieve the smallest
sufficient evidence from its existing owner before asking.

Bound inputs explicitly. Preserve omission, unavailable output, stale source and
uncertainty as facts; never silently replace them with empty or successful evidence.
Keep deterministic admission, provenance, counts and limits in code. Use shared
state and one packet for related judgments when they need the same evidence.
A typed answer only establishes shape; its meaning remains an empirical claim.

Probe counterexamples as well as happy paths: identical IDs with different source
content, irrelevant changes, omitted/binary content, ambiguous diagnostics,
conflicting scope and instructions embedded in evidence. Inspect live requests and
replay real responses through the production interpreter. Record what was tested,
what was only compiled and which operational consequences remain unverified.
Retain that audit with the implementation handoff so later RSI passes can improve
both the question and the evidence acquisition, rather than tuning thresholds over
an inadequate packet.

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

### Recursive delegation experiment

The execution workflow is live scaffold → unfold → checked integration → next
ready batch. WorkPlan's preauthored graph is removed. Sol holds cross-component
choices; Luna owners recursively define shared boundaries and delegate ready
implementation. Target at least three Luna implementation levels on average,
with terminal-leaf justification instead of a mandatory fork count. Reviews do
not count toward implementation depth. Nested review checks leaf changes and
component joins at their respective boundaries.

The hypothesis is shorter accepted-delivery wall time through parallel work at
several depths, plus fewer parent relay rounds through checked ReviewFlow and
bounded relays of existing decisions. Measure useful depth, overlap, time to
first useful fork, blocked dependency time, source corrections, reviewed defects,
parent relays and accepted-delivery time from existing run evidence. A deeper
tree is not success if the same work is serialized or repeatedly re-reviewed.

## Method: reconstruct the workflow before tuning its mechanisms

Apply this method in every RSI pass and every audit analysis. Start with the
intended outcome and reconstruct the whole logical flow: what happened, why each
step existed, what each participant knew, who owned the next action, and what
event could advance the work. Follow dependencies and feedback across actors,
tools and runtime boundaries. The location of a symptom is the starting point
for investigation; establish where the workflow actually went wrong.

Before choosing a repair, describe how the work could happen differently. Consider
removing a step, changing admission order, moving responsibility to its proper
owner, retaining a continuation, or replacing repeated model work with authored
machinery. Compare these alternatives with tuning the existing mechanism. Use
evidence from earlier waves to test whether the same structural problem recurs.
Keep causal hypotheses separate from observations and seek valid counterexamples.

Match the lens to the suspected relationship. A dependency graph exposes blocked
work and critical paths; a state machine exposes missing transitions or ownership
transfer; queueing and backpressure suggest comparing arrival, admission and
service times under measured load. Information-flow analysis asks whether the
recipient had the evidence needed at its decision point. Counterfactual removal
asks which useful outcome would disappear with a step. Combine these lenses
when one mechanism explains several symptoms; transfer the mechanism to analogous
workflows rather than searching only for repeated wording in traces.

For example, repeated reminders to a worker awaiting a parent's interface decision
require tracing preparation, admission, the question, ownership of the answer,
delivery and resumption. Decide how that dependency should work before adjusting
reminder timing or wording. A bounded containment fix may still be useful; record
which underlying design question it leaves open.

Each audit recommendation should name the intended flow, the observed divergence,
the alternatives considered, and why the chosen intervention improves the whole
flow. Verify the real consumer and failure paths, then observe the next wave for
costs shifted elsewhere. Scale the investigation to the consequence; a clear local
defect can justify a local fix without inventing a larger framework.

## Core target: improve the graph of model rounds

First ask whether a step needs to exist. Prune mechanisms whose observed cost
is high and whose benefit is unsupported, including generic judgments invoked
after every tool call. Prefer an explicit task event and a narrow question.
Recent conversational context should select non-tool messages within a total
budget; add tool evidence deliberately for the judgment that needs it. A limit
on turns alone does not bound a transcript with hundreds of tool calls.

For semantic automation, execute obvious authorized cases and interrupt the
frontier model on uncertainty. An uncertain answer does not authorize another
investigation loop. A diagnostic probe is useful when selecting that probe is
itself an obvious, bounded step. Explore, exploit and prune across iterations;
do not turn these experiments into a commitment to one complete framework.

### Distill small decision flows into actors

A core RSI goal is to distill recurring decision trees into Haskell effects and
small flowchart-style actors, using Jev for semantic branching. Start with the
mind-numbing bookkeeping: one repeated five-tool-call sequence, a routine relay,
or the first diagnostic reads after a failure. Choose a narrow episode with a
clear input, useful output and stopping point. Replacing that episode is enough;
do not expand it into replacing the agent's whole engineering assignment.

Draw the observed flow before implementing it. Code handles exact conditions,
identities and transitions; Jev interprets bounded evidence where choosing the
next branch needs semantic understanding. The actor retains the original handles,
intermediate evidence and continuation across events. Its Haskell interface lets
the frontier model supply context, permitted actions and decision criteria once,
then receive the result or the specific question the flow could not settle.

For example: a command fails, the agent reads stderr, chooses a diagnostic read,
reads its output and summarizes what to do next. A small actor can retain the
failure, let Jev select among supplied read-only probes, execute a bounded probe,
and return the original outcome with useful diagnostic evidence. The agent still
owns the repair. Similarly, a flow can deliver a baseline update and collect
incorporation evidence while leaving semantic conflicts with the component owner.

Prefer a small complete flow over a collection of wrappers that still requires
the model to perform every transition. Reuse existing effects, routing callbacks
and resource owners. Give semantic branches an unresolved outcome and preserve
evidence for escalation. Validate representative successful, failed and ambiguous
episodes, then measure setup, judgments, recovery and actual frontier work saved.
Keep useful partial coverage: automating a modest fraction of opportunities can
pay off without generalizing the flow to every case.

Audit the graph of work across model rounds, tool calls, forks, replies, review
and integration. The worker tree alone is insufficient: children return evidence
and decisions to parents, and repairs and follow-ups create further dependencies.
Find small recurring sequences where authored Haskell can save frontier model
rounds, improve results, or supply a bounded stronger judgment to a cheaper node.

Move exact operations into deterministic code and contextual decisions into
bounded Jev judgments. Use a focused Astra consultation when consequential
uncertainty needs frontier reasoning; retain its useful policy or procedure in
Haskell where possible. This can give Luna nodes access to stronger judgment at
the needed boundary without upgrading every round. A compiled procedure does not
itself establish Astra-level judgment quality: evaluate the resulting decisions.

Start with modest, composable wins. A follow-up that saves a few frontier rounds
in roughly 30% of opportunities can justify a cheap judgment even if that judgment
runs on every opportunity. That fraction is an illustrative payoff, not a quota
or measured rate. Preserve a useful fallback for the remaining cases. Accumulate
several proven improvements instead of requiring the first workflow to cover all
cases. Include useful vigilance and coordination that agents previously omitted
because performing it through model rounds cost too much.

For each intervention record:

- The triggering opportunity and actual sequence of calls, including information
  returned from children and any subsequent parent work.
- The replacement Haskell workflow, exact operations, semantic judgments and
  point where an unresolved decision returns to a model.
- Observed use, rounds avoided, output/context reduction, result quality and
  newly feasible work; keep estimated savings separate from measured outcomes.
- Added judgment, setup, latency and maintenance costs, failed follow-ups and
  fallback behavior. Evaluate combined improvements without double-counting the
  same avoided round or hiding costs shifted to children or the supervisor.

Evaluate the Haskell surface alongside the workflow. Can an agent discover,
compose, customize and inherit it without reconstructing bookkeeping? Do its
types and combinators express the useful logic, retain evidence and make failure
handling clear? Does the eDSL operate concrete machinery with a small, stable
interface? Improve the offered helpers and frameworks when observed use exposes
leaky boundaries, awkward composition or needless ceremony. Interface elegance
must help real consumers express richer behavior, not merely add wrappers.

Give validated, advertised automations at least three waves of actual prompt
exposure before judging non-use. Distinguish absent opportunities, discovery
problems, setup failures and poor utility; fix confirmed defects immediately.

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

Before interpreting a quiet run, check opportunity coverage and sensitivity:
could this workload reach the behavior, did the relevant prompt or helper reach
the actor, and would the trace or evaluator detect the consequence? Use retained
known failures or controlled private replays to test the observation path. A
missed positive control calls the procedure into question, not the product.

For behavioral trials of prompt and skill changes, inspect decisions and
artifacts. Compare a representative task, an analogous task with different vocabulary, and a task
where the proposed method does not fit. Where useful, ablate a cue while holding
the task, capabilities and evidence comparable. Word counts and expected phrases
do not establish better behavior. An editorial walkthrough generates hypotheses
for these trials; it does not substitute for execution.

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

Audit opportunities and successful behavior alongside failures. Look for repeated
command/read/judgment sequences that could become a useful notebook function,
recurring relay that an actor could handle, and helpers that descendants could
reuse. A missed abstraction is a counterfactual proposal, not an observed defect:
name its actual consumers, construction/validation cost, expected reuse and the
evidence that it would improve this workload. Do not equate more abstractions
with better work or penalize a sensible one-off command.

For abstractions that were created, trace actual use: which calls or model rounds
it replaced, who reused or customized it, how it preserved failure evidence, and
what maintenance or discovery cost it introduced. Distinguish defined, compiled,
executed, reused and inherited successfully. A concise function that nobody used
is not demonstrated value. Investigate successful compositions to identify what
enabled them—an example, available primitive, shared prefix, task shape or actor
initiative—and promote those conditions through remixable seeds, prompts and
cleaner APIs. Retain counterexamples where the abstraction cost more than it saved.

Use bounded subagent audits for both missed opportunities and positive examples;
compare them across actors before generalizing. The next experiment should test
whether the proposed affordance produces useful adoption, rather than merely
whether actors comply with an instruction to create a helper.

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

Use matched before/after workloads and repeated observations when variability
could change the decision. Distinguish a mechanism supported by an intervention
from correlation with host load, task difficulty, or model variation. Preserve
failed and incomplete attempts in the comparison. Continue only investigations
that could change an open decision; close an experiment with an unresolved
verdict when its evidence cannot discriminate, while keeping unmet product
obligations visible and owned.

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

The method remains subject to the same observation and revision as the
orchestration policies it evaluates.
