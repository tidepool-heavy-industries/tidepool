# More useful swarm work through Jev-driven programs

Research priority: [learn and adapt TypeSafe’s published patterns first](jev-typesafe-patterns.md).
Let observed capabilities reshape the program; the experiment shortlist below
is revisable, not a prescribed role for Jev.

Status: strategy and experiment plan, 2026-09-17. The ambition is **10× useful
swarm activity for the same total model spend**, with acceptable quality and
elapsed time. This is a direction to pursue, not a measured result or a promised
speedup. It does not change the active run's assignment.

Companions: [shared System 1 components](jev-shared-components.md) describes
agents exchanging and improving code; [recovered experiments](jev-recovered-experiments.md)
provides the individual application ideas. This document explains how they
could combine, where the gains might come from, and how to choose experiments.

## The leverage model

Spend includes planner/author calls, workers, reviewers, retries, Jev, and the
operator's model-assisted repairs. Useful activity means checked product changes,
answered investigations, or reusable components that another consumer actually
uses. More actors, calls, or commits alone do not establish progress.

The opportunities reinforce one another:

1. Authored code removes routine model turns.
2. Evidence preparation reduces the input and investigation needed by remaining calls.
3. Better-prepared obligations can become tractable for smaller models.
4. Shared components amortize strong-model design across tasks and runs.
5. Local supervision allows more concurrent useful work without expanding the
   root's coordination burden proportionally.

These gains overlap; do not multiply isolated estimates as if independent.
Dropping one verbose call can save both that call and some input on subsequent
calls. Conversely, cheap decisions can cause expensive extra builds or repairs.
The unit to inspect is the completed work, including those consequences.

For one component, the practical break-even question is: do avoided calls and
rework across its actual uses repay authoring, adoption, Jev, and exception costs?
A one-use component can still be worthwhile if it unlocks a task. Reuse is an
opportunity, not an obligation to generalize every successful expression.

## Critical enabler: access to the agent’s own working context

Make it easy for authored Haskell to reference the calling agent’s recent
conversation, including available native tool interactions. This supplies intent,
corrections, and attempted approaches alongside exact artifacts, without another
model turn spent retelling them. Treat it as a major part of the 10× ambition.

The [own-context experiment](jev-own-context.md) belongs in the next Codex-driven
direct-use runs. Opus can prototype consumers, but its proxy cell history does not
prove access to a Sol node’s conversation. Use the existing conversation owner,
a bounded snapshot, and explicit evidence/context fields; no new memory subsystem.

## High- and low-leverage Jev patterns

The easiest demonstration is not necessarily the best use. Picking one obvious
search result proves the plumbing; it may save almost no work. Assess leverage
at the level of the consuming program: what useful work now happens without
another larger-model turn, and what context no longer has to travel through it?
Classification and selection can be powerful when their results drive substantial
work. A sophisticated question that nobody acts on can have very little value.

### Strong candidates for high leverage

| Pattern | What makes it valuable | Example for one Sol and its Luna workers |
| --- | --- | --- |
| Rich semantic observations, several consumers | One evidence packet answers distinct useful questions; code reuses those judgments for selection, routing, and context. | Classify relevant diagnostics, match existing examples, identify missing evidence, and distinguish local defects from shared-contract questions; use the answers to construct the repair brief and next probes. |
| Feedback-driven investigation | New observations change subsequent choices, letting an authored program do multiple useful steps without returning after each tool call. | Search candidates, read promising definitions and consumers, inspect a test, then return an evidence-backed answer or the remaining question. |
| Broad semantic work over collections | Many small judgments replace repeated scanning by a larger model, and their typed outputs can be reused. | Relate a set of changed interfaces to callers and requirements, or distinguish genuinely relevant output blocks across a large retained result. |
| Small frontier of plausible paths | Ambiguity produces useful exploration rather than immediate commitment or escalation. | Follow two plausible source paths, gather observations on each, and retain unresolved investigation nodes. |
| Semantic action selection in an authored controller | Available actions, current observations, and recent consequences are explicit; the chosen action actually advances work. | A local repair controller selects a useful probe or a prepared Luna assignment, checks the resulting candidate, and handles familiar follow-up work. |
| Reusable semantic functions | Strong-model effort on state preparation, questions, and result handling is reused and improved by other consumers. | A mechanism-finding component serves both a direct Sol investigation and a Luna repair preparation program. |
| Frequent tool-boundary improvements | A modest per-interaction gain can recur widely and avoid accumulating irrelevant context. | Relevant original output blocks appear first, with omitted material accessible through retained-output reads. |

Question packets can include useful branch-conditional questions whose answers
code ignores when that branch is irrelevant. Questions in the same request do
not read each other's answers: state a premise explicitly or ask again after new
evidence. More questions are justified by their consumers, not by a density quota.

Give candidates meaningful structure. A source candidate can include its
signature, relevant body, and role; a proposed probe can name the observation
it obtains and the alternatives it could distinguish. The model should have
enough evidence to compare candidates, rather than guess from names. Code keeps
the actual source references, commands, and callable payloads locally.

### Lower-leverage or misleading uses

- Asking Jev to interpret literal exit codes, count overlaps, enforce known
  precedence, or choose a lifecycle transition that the program already knows.
- Making a larger model initiate and interpret every tiny Jev classification.
  This adds another interaction unless the answer itself saves meaningful work.
- Asking one vague global question such as "is this correct?" and treating a
  concentrated distribution as coverage of the whole contract.
- Issuing a serial request for every independent field already answerable from
  the same state, or fetching the same evidence repeatedly.
- Asking many unused questions merely because judgments are cheap. Avoid turning
  possible low marginal latency into a claim of unlimited free computation.
- Repeatedly judging unchanged state without a specific consistency experiment.
- Asking a generic next-step question over an undifferentiated transcript, with
  no concrete action candidates or current-world evidence.
- Building a general controller framework before a task-specific program has a
  real consumer. Authoring and repair effort are part of the cost.

Single-hop selection is not inherently low leverage: finding a key definition
among hundreds of hits can be excellent. Similarly, output filtering may be
valuable at high frequency even though it does not run a long decision loop.
The distinction is useful work and attention saved, not the number of Jev calls.

### Revised guidance for the next direct-use runs

Keep the selected run shape: one Sol, Luna trees available, a medium-sized task.
Supply at least one worked shape that demonstrates substantive composition,
alongside the smallest syntax examples. For example:

1. Enumerate real source candidates for a question.
2. Ask several useful relevance/relationship questions over the evidence.
3. Read selected source and production uses through ordinary commands.
4. Ask again over the new evidence and retain more than one path if useful.
5. Return a focused finding, a well-prepared Luna request, or a precise unresolved
   question, with references to what was inspected.

This can be an ordinary function called by Sol; autonomous supervision is not
required. It tests the interesting capability while keeping the run operationally
simple. Adapt the example to the real task rather than demanding this sequence.

Do not call evidence preparation the uniquely best Jev application. It is one
promising, inspectable candidate. Tool-result filtering, collection-wide semantic
work, and state-aware worker control should compete on actual utility. A failed
question formulation is not a universal verdict on the pattern.

The live TypeSafe guidance supports independent/speculative questions over one
state, structured criteria, and multi-path hierarchical traversal. These are
patterns to borrow, not evidence for their quality on our coding tasks:
[fan-out](https://docs.typesafe.ai/patterns/fan-out.md),
[structured criteria](https://docs.typesafe.ai/primitives/advanced.md),
[hierarchical classification](https://docs.typesafe.ai/cookbooks/hierarchical_classification.md).

## 1. Jev-driven supervision around small workers

### Shape

An actor retains the assignment, current candidate, outstanding request,
observations, and unresolved conditions. Code performs known lifecycle steps.
At semantic branches, Jev evaluates the current evidence and available actions.
The actor can gather more evidence, request a Luna repair, rerun a relevant
check, or return an architectural question to Sol.

The worker generates code. The controller keeps the work moving. The existing
runtime supplies actor admission, process/resource handling, and worktree ownership.
A supervisor and a worktree-owning merge actor may remain separate where current
ownership requires it; a useful loop does not require collapsing their authority.

### Options

- **Fixed loop, semantic predicates:** run the known sequence and use Jev for
  conditions such as whether a finding is an implementation defect or a contract
  gap. Easiest to inspect and close to the current review/repair work.
- **State-aware action selection:** at an open investigation state, offer actual
  available probes or bounded repair requests. Jev selects a useful action from
  that set. More flexible; needs good state and candidate construction.
- **General next-step prediction:** ask what the agent should do from a transcript.
  Current local trials found this weak. Do not make it the default controller.

Use the first form for lifecycle progression and the second inside investigation.
They compose naturally. Jev should not rediscover that a settled repair must be
checked; it can help choose which observation would explain a new failure.

### First experiment and forks in the road

Start with one supervisor doing one real task through a repair cycle. Fable may
manually provide the initial candidate and the available diagnostic actions.
Prove the authored path executes before scaling the tree.

- If Sol still supplies obvious transitions, move those transitions into code.
- If Luna receives vague repair requests, improve evidence preparation first.
- If Jev repeatedly picks the wrong branch, inspect option conditions and missing
  state before adding a stronger model to every decision.
- If the task contract itself changes, return that decision to its owner.

A settled request needs a new repair request. Keep candidate and attempt identity
so delayed results cannot advance a newer attempt. Existing review/publication
policy stays explicit; fewer model turns must not silently mean unchecked merges.

## 2. Prepare expensive calls with cheap investigation

### Shape

Before a worker is asked to reason, a program gathers the actual failure,
relevant source, a production consumer, available tests, and any applicable
decision or migration example. Jev selects meaningful relationships among those
observations. The worker receives an evidence-backed question with references
to the full retained material.

### Options

- **Literal preparation:** extract compiler spans, exit status, changed files,
  and named test failures without inference. This is the baseline.
- **One semantic hop:** select the relevant source hit or working example.
  Good first addition because it has an inspectable result.
- **Several evidence-dependent hops:** follow a caller, inspect a contract,
  find a matching migration, then choose a discriminating check.
  Worth trying after the one-hop path helps.

Ask independent questions over the same evidence together when useful. Ask again
after obtaining evidence the earlier request could not inspect. Do not make
every helper perform the maximum investigation before it lets a worker begin.

### First experiment and forks in the road

Wrap one recurring failure path with a small diagnosis function and use its
output as Luna's assignment. Ask the worker what it still needed to retrieve
and which supplied details were irrelevant.

- If the first selected span is useful, add one further hop where workers
  repeatedly need it.
- If preparation takes longer than the investigation it replaces, shorten it.
- If it finds a shared-contract problem, save the wasted repair round and ask
  the owner directly.
- If it cannot distinguish explanations, return them with the evidence. A
  better-prepared unresolved question is a successful partial result.

This is also the easiest place to try conditional preparation after a check
fails. Begin with an explicitly called helper; background triggers can follow.

## 3. Keep working data out of model history

### Shape

Full command outputs and structured observations remain available through the
existing owners. Code extracts exact facts. Jev judges relevance or relationships
where parsing cannot. Return selected excerpts, contradictory evidence, source
identities, and an indication of omitted material.

### Options

- **Projection:** show a few fields or exact errors. Cheap and deterministic.
- **Selection:** choose relevant original spans. Jev adds meaning while keeping
  the source text recoverable.
- **Generated summary:** potentially useful, but adds a generative call and can
  erase important distinctions. Use where the consumer actually needs synthesis.

Prefer projection and selection for routine output. Required facts such as a
failed check or publication mismatch must not depend on Jev judging them relevant.
Preserve access to full evidence so a reviewer can challenge the selection.

### First experiment and forks in the road

For one worker, supply selected diagnostics instead of full build output. Check
whether it proceeds correctly, asks for more, or repeatedly misses context.

If the worker routinely expands everything, the selection is not yet useful.
If it never expands but makes errors from omissions, the selection is misleading.
Preserve source excerpts and contradictory examples when adjusting the helper.

Do not assume exact-context forks inherit a filtered history. Selecting evidence
for a fresh bounded worker and reducing new noise in an existing context are
different operations. Use the current context mechanisms rather than inventing
selective inheritance for this experiment.

### Tool-result experiment: semantic omission with expansion

A concrete user proposal: ordinary tool calls can ask Jev whether particular
output blocks can be omitted as noise from the initial model-facing result.
The complete output stays retained; the model can expand omitted context using
the existing output-reading/pagination mechanism. This could improve many
interactions without requiring the agent to remember a filtering helper.

Code divides an eligible result into addressable blocks. Given the operation's
purpose, the immediate inquiry when available, and the candidate blocks, Jev
judges what is relevant to display. Code renders the selected original text and
compact omission markers, preserving order and a route to the full result.
The operation's exit status and other required result fields remain visible.

Try one noisy output family first, such as repeated build progress surrounding
diagnostics. Compare what the next worker actually reads and whether it expands
the omitted material. Keep short outputs on the cheap direct path. If Jev is
unavailable, ordinary existing output rendering still works.

Existing pagination may page a sequential stream rather than arbitrary omitted
ranges. Verify its actual retrieval contract. Reuse stable output offsets or
the full-output handle where possible; do not advertise a per-gap expansion
that the current API cannot perform. Expansion must read retained output rather
than rerun the original command or resample the judgment.

The criterion should name the current purpose. "Noise" is not an intrinsic
property of a line: a routine warning can matter to a dependency investigation.
Avoid isolated per-line judgments that strip a diagnostic of its surrounding
explanation. Preserve useful blocks and explicitly mark incomplete source
capture; retained-but-hidden and never-captured are different cases.

### Aggressive, context-aware views with a revision loop

Recoverable omission permits aggressive experiments with what the agent initially
sees. Combine the [agent’s own recent context](jev-own-context.md) with the output
and current inquiry: foreground changed evidence or contradictions, collapse
repeated diagnostics, and show subsystem detail when that is what the agent is
investigating. Optimize useful attention rather than the number of hidden bytes.

Pagination makes recovery cheap once the reader knows something exists. Therefore
render a compact omission index: the kinds of blocks collapsed, their extent, and
stable retrieval references. Prefer mechanically derived labels where available;
semantic labels are hints, not a substitute for the original material. An opaque
“more output available” marker is insufficient for aggressive selection. The
retained source must outlive the view’s promised retrieval window; expose expiry
or incomplete capture rather than claiming everything is recoverable forever.

The improvement loop is ordinary project development:

1. Retain original output and the relevant context snapshot/reference.
2. Apply an authored selection function and render a discoverable partial view.
3. Let the agent expand blocks through existing reads, without rerunning tools.
4. Note useful expansions, misleading labels, and omissions that delayed or changed
   a decision. Save a few revealing examples with the selection-function revision.
5. Have the agent or supervising developer revise the Haskell/question bundle,
   try those examples and a different output, and use the revision in later work.

An expansion is not automatically a selection failure: on-demand detail is the
point. Strong failure signals are “I could not tell this evidence existed” and
“that omitted block would have changed my decision.” No expansion is not proof
of success either. Interviews and occasional inspection of the complete output
can reveal unnoticed omissions; no large evaluation campaign is required.

Begin with one output family to keep the trial interpretable, but do not restrict
Jev to obvious redundancy. Try meaningful task-dependent selection once retrieval
and omission discovery work. Policies can be shared as source and improved between
runs; live policy mutation and a new telemetry service are not prerequisites.

This moves selection to a high-frequency surface. Its Jev latency and extra
inference cost must be repaid by less input and less reader confusion. It needs
no new frontend: the useful behavior is in the existing tool response and
expansion path. Existing unfiltered rendering remains the fallback.

## 4. Accumulate reusable System 1 code

### Shape

A strong-model intervention produces a fix and, when worthwhile, a reusable
semantic function. Another agent tries it, adapts it, and returns an improvement.
The accumulated artifact is code plus examples and limits, not just a collection
of prompt fragments.

### Options

- **Source in a short note:** lowest friction; good for a rough discovery.
- **Project Haskell at the next restart:** checked, repeatable, and easy to
  distribute through the existing frozen project package. Recommended first.
- **Live callable exchange:** interesting when peers already need a component
  during the same run. Verify the combined Jev/function-transport path separately.

Start with a function-specific signature and explicit inputs. A universal
component interface is premature. Keep task data separate from reusable criteria
where a second consumer needs that distinction. A function specialized to one
repository can still be valuable.

### First experiment and forks in the road

Follow the shared-components plan: author, second consumer, revision, later use.
Replay the earlier example when changing the function, and retain the new case.

- If nobody discovers the saved component, nominate a consumer manually; test
  usefulness before building discovery machinery.
- If it needs substantial explanation every time, improve the signature,
  example, and evidence requirements.
- If consumers need different policies, parameterize that difference or keep
  explicit variants. Do not force a single supposedly universal threshold.
- If a revision helps one case and harms another, preserve both observations
  and decide whether the component needs a narrower purpose.

Sharing also spreads mistakes. A common component should be easy to inspect and
replace; agreement among several users of the same component is not independent
confirmation of its judgments.

## 5. Explore cheap branches before spawning expensive ones

### Shape

Keep unresolved questions as ordinary data with evidence, candidate probes,
actions already attempted, and a status. Run a loop that expands useful questions,
gathers observations, and delegates only branches requiring generation or
substantial independent reasoning.

This can resemble an unfold from a seed. A simple list or map and loop are
sufficient for the first trial; recursion-scheme machinery is optional.

### Options

- **One selected probe at a time:** easiest to understand; may wait unnecessarily
  when two independent reads are both useful.
- **A small set of plausible branches:** retain alternatives and gather separate
  observations before committing to an explanation.
- **Broad speculative expansion:** potentially faster but can overwhelm tools,
  build resources, and the eventual consumer. Only expand where independent
  observations have a plausible benefit.

Choice mass is relative to the offered alternatives. Do not compare it across
unrelated questions as a universal priority score. Literal priority constraints
stay in code; semantic importance can be asked as a separate, described judgment.

### First experiment and forks in the road

Use a real failure with several plausible explanations and a handful of available
probes. Keep unfinished questions in a file if they must survive a restart.

If one cheap observation settles it, no worker is needed. If several explanations
survive, Luna gets those explanations and their observations. If new evidence
shows the candidate set was wrong, ask a generative model to revise it rather
than repeatedly choosing the least-wrong old option.

An unresolved node is useful only if it says what remains unknown. A growing
backlog with no path to resolution is not increased useful swarm activity.

## 6. Escalation that improves the next attempt

### Shape

An escalation carries the objective, exact candidate, observations, attempts,
unresolved distinction, and suggested next evidence. The larger model can solve
the case without reconstructing the entire run. It may then improve the controller.

### Options

- **Immediate escalation:** appropriate for authority or shared-contract choices
  the controller is not meant to decide.
- **One more cheap observation:** appropriate when a named missing fact can be
  obtained through an available operation.
- **Luna investigation first:** appropriate for local source reading or generation
  that does not require the parent's architectural judgment.

Use recent events as evidence for semantic conditions such as repeated variants
of the same failed assumption. Exclude ordinary waiting and known infrastructure
failures in code. Evaluate after relevant new events rather than polling unchanged
transcripts. A completed observation should be reused, not fetched on every tick.

### First experiment and forks in the road

Have a controller write one proposed Luna brief when it detects recurring trouble.
The operator can launch it manually. Then connect the useful condition to actor
admission with the existing budgets and request ownership.

If escalation repeats, inspect whether the larger model's answer ever reached
the controller's state or source. If the new branch is overly specific, retain
the example and leave the decision with the model. Every exception need not become
automation; the recurring useful ones are the investment.

## Combinations worth trying

| Combination | Why it could be more useful together | Small first trial |
| --- | --- | --- |
| Supervision + diagnosis + Luna | The controller removes coordination turns; diagnosis makes a small worker more effective. | One checked candidate, one repair, a typed outcome to its parent. |
| Output selection + fresh bounded worker | Fewer irrelevant inputs may reduce both spend and confusion. | Give one worker exact relevant evidence plus access to the rest. |
| Tool-result filtering + existing pagination | Routine calls stop adding avoidable noise while full evidence remains available. | Hide redundant blocks in one output family; retrieve them through the retained output handle. |
| Shared components + matching past decisions | The system can retrieve executable help as well as prose explanations. | Manually supply a saved function and a governing decision to a second task. |
| Migration archaeology + affected-assumption checks | A shared change yields a working migration example and focused follow-up obligations. | One API change, two consumers, an already-migrated caller. |
| Executable criteria + counterexample reduction | Failures become small actionable repair inputs that travel between agents. | Reduce one confirmed failure and attach its checker to the repair. |
| Semantic watcher + retained investigation nodes | Related failures can become one focused investigation instead of repeated parent interruptions. | Combine several real events, propose one Luna request, inspect it manually. |
| Cheap branches + independent workers | Initial probes sharpen the distinct questions worth parallel reasoning. | Investigate locally, then delegate only two genuinely different surviving obligations. |
| All of the above + shared-component iteration | A resolved exception improves later controllers, amortizing the strongest reasoning. | Adopt one demonstrated improvement at the next restart and have another agent use it. |

## Game out a representative loop

A Luna's candidate fails a check. The supervisor parses the literal result and
retains the output. A semantic selector identifies which source and existing
example are relevant. The next action depends on the resulting evidence:

- **Familiar local defect:** issue a fresh repair request with the example and
  failing check. Validate the new candidate through the normal policy.
- **Missing evidence:** run the selected probe and reevaluate against the changed
  state. Do not ask the same question again without a reason.
- **Ambiguous mechanism:** retain the plausible explanations and try cheap
  discriminating observations. Delegate substantial remaining investigation.
- **Contract gap:** send Sol the exact contradiction and affected consumers.
  Independent work can continue if its premises still hold.
- **Unavailable tool/provider:** expose the actual failure to its existing owner;
  do not disguise it as a semantic doubt or worker defect.

Sol resolves a genuine gap, perhaps by finding an existing migration pattern.
It saves a useful source-selection or contract-checking function. At the next
restart a different supervisor uses that function on another candidate. The
second use tests whether we accumulated capability or merely saved a transcript.

## Where the ambitious approach could disappoint

- **Authoring overhead dominates.** Keep experiments project-local; manually
  assemble inputs before designing abstractions. Reuse Fable-authored shapes.
- **Selection hides the important fact.** Restore the omitted evidence and make
  that case part of the component's examples; do not simply increase confidence.
- **Small workers still need larger-model reasoning.** Improve the obligation or
  change model placement. Cheap supervision cannot make every task Luna-sized.
- **Controller decisions are good but actions are slow.** Look at compilation,
  command startup, builds, and resource contention. More Jev will not fix these.
- **More parallelism reduces throughput.** Spend additional capacity first on
  independent reads and bounded work; respect shared build and memory limits.
- **The same error propagates through shared code.** Keep the definition and
  cases visible, preserve an independent check appropriate to the task, and
  make replacement ordinary source editing.
- **Everything becomes an investigation node.** Resolve one recurring class or
  simplify the controller before expanding the tree.

## Suggested order for the next experiments

1. For the next one or two runs, use one Sol on a medium-sized real task with
   Luna subagent trees, explicitly encouraged to leverage Jev. Follow the
   [current run direction](jev-tool-improvement.md). Prioritize smooth direct
   use and useful work; no additional Sol coordination levels are needed.
2. In parallel, run the [own-context integration experiment](jev-own-context.md)
   from a Codex node. Let useful Haskell functions reference recent conversation
   without asking the agent to reconstruct it.
3. Fix crashes and significant friction, interview the agent, and rerun with
   improved tools/examples. Save useful System 1 discoveries immediately.
4. Give a saved component to another consumer, including a Luna child, and
   incorporate an improvement through ordinary source at the next restart.
5. Once this baseline is comfortable, automate a repetitive sequence observed
   in those runs. Prove the authored supervisor through completion and repair.
6. Add one semantic watcher or investigation frontier where the preceding run
   actually needed it. A manually executed proposed action is an acceptable MVP.
7. Increase concurrent obligations when local completion is useful and resource
   capacity permits. Try live callable sharing when source exchange reveals a
   concrete reason to need it during a run.

These are learning steps, not release gates or a mandated model topology. Work
that naturally combines two steps can do so in one run. Fable authors and repairs
the difficult controller design; agents should have room to adapt useful pieces.

## Lightweight evidence for the 10× ambition

Use existing run observations and interviews. For each attempt, keep useful
completed outcomes, total model spend, larger-model interventions, elapsed time,
and conspicuous rework or compute bottlenecks. Include setup/authoring spend and
report when its reuse cost is being amortized across runs.

Ask: which expensive turn disappeared, what work replaced it, and did the
consumer still get what it needed? Was a smaller model now able to finish the
obligation? Did a later run actually benefit from a saved component?

Comparable tasks can give rough directional evidence. Different task difficulty,
cache behavior, and manual assistance should remain visible; do not turn an
impressive individual run into a universal multiplier claim. Ambition should
drive what we attempt, while concrete outcomes drive what we retain.

## Research connections

- Exomonad's `future_work/stream-detection.md` proposed parent-defined semantic
  watchers when continuous classification was expensive. Jev changes that cost.
- Its `docs/decisions/spec-commits.md` combined types, intent, and executable
  acceptance in a handoff. Recover that composition, not its old role rules or
  assumptions about context inheritance.
- TypeSafe's [state guide](https://docs.typesafe.ai/concepts/state.md) and
  [fan-out pattern](https://docs.typesafe.ai/patterns/fan-out.md) inform compact
  current-state decisions and independent questions over one observation.
- Its [hierarchical classification cookbook](https://docs.typesafe.ai/cookbooks/hierarchical_classification.md)
  supplies a useful frontier-search analogy. It does not establish software-repair
  accuracy or make path scores calibrated correctness probabilities.
- The [DOOM description](https://typesafe.ai/blog/introducing-system-one-models-and-jev)
  motivates reactive action selection from structured state. The research found
  a vendor description, not inspected public controller code. We must develop
  and try the coding-controller state and actions ourselves.
