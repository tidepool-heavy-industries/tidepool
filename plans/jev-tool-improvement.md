# Continuous improvement of the agent's tools

Research priority: [learn and adapt TypeSafe’s published patterns first](jev-typesafe-patterns.md).
Let observed capabilities reshape the program; the experiment shortlist below
is revisable, not a prescribed role for Jev.

Status: guiding plan and critical path, 2026-09-17. The user describes the
core idea as kaizen: many small improvements to the tools agents think and
act through, alongside ambitious experiments in Jev-driven actors.

## Product direction

Make the next interaction work better: less irrelevant output, better evidence,
fewer awkward steps, more useful types, and reusable semantic functions.
Haskell makes improvements executable and composable; Jev makes some of them
sensitive to meaning rather than fixed syntax. Actual use reveals the next
rough edge. Agents and the human help reshape the environment between runs.

The ambition is 10× useful swarm activity for the same total model spend.
Small improvements and autonomous actor loops are complementary ways to pursue
it. The first can benefit many existing calls; the second can remove whole
classes of coordination calls. Neither benefit is established merely by using Jev.

The broader product is an experimental substrate for agent-authored programs.
A useful, reshapeable swarm is the starting application. Setup can be rough in
v0; powerful working interactions, compelling README examples, and approachable
effect authoring matter first. No new frontend is required.

## The plan set

| Document | Purpose |
| --- | --- |
| [Swarm leverage](jev-swarm-leverage.md) | Six strategies, options, combinations, failure cases, and the 10× ambition. |
| [Shared System 1 components](jev-shared-components.md) | Agents author, exchange, use, improve, and reuse Jev-backed Haskell code. |
| [Recovered experiments](jev-recovered-experiments.md) | A menu of diagnosis, attention, decision reuse, executable criteria, and code-exploration trials. |
| This document | How to choose the next improvement and the shortest route to useful adoption. |

These plans are connected guidance, not four independent implementation programs.
Choose the next small intervention from real work; do not commission every idea.

## The working improvement loop

1. Notice a concrete interaction that wasted time, tokens, or attention.
2. Save enough of the actual case to reproduce or discuss it.
3. Change the smallest useful piece: source, a question, a type, a projection,
   a diagnostic, a skill example, or an authored actor handler.
4. Exercise it through the same real consumer that encountered the problem.
5. Let another agent use it. Ask what it still had to reconstruct or work around.
6. Keep, revise, or discard it. Reuse recurring improvements in project source;
   change a runtime primitive when the problem belongs to its owning mechanism.

Files, Git, manual handoffs, and frequent restarts are enough. Saving a component
must be cheaper than losing the lesson. Use a short note when extracting a
polished module would interrupt productive work.

Favor improvements that recur, have a clear consumer, and are easy to try. A
high-frequency annoyance can be a better investment than a more impressive
feature. Preserve room for unusual experiments when their potential is large.
Do not turn this into a scored backlog or a mandatory reporting ritual.

## Current evidence that changes the starting point

The recorded dry run before run 8 in [dogfood notes](jev-dogfood-notes.md)
reports that `gateFor` starts on the prepared route, a green candidate publishes,
and a red candidate restores the worktree without advancing publication.
Those are important pieces already demonstrated. They do not establish that the
new complete supervisor handles worker settlement, fresh repair requests,
semantic decisions, and eventual publication together.

For output filtering, existing command reads support original byte positions
and retained-output recovery. The command skill documents `read_output` and
`next_offset`; `CommandJobs::read` accepts offsets and slices. This is promising
substrate for expandable omissions. It does not yet establish how a semantic
preview should describe noncontiguous omitted blocks to its consumer.

Record-actor command handling also has a concrete constraint: an ordinary
`Cmd.run` observation timeout fails the handler. Long unattended work should
use the existing start/completion path rather than assume an interactive
recovery binding appears in a background actor.

These are source/document spot checks, not a new acceptance run. Consult the
active implementer's latest evidence before scheduling work already completed.

## Next one or two runs: one Sol with Luna trees

User-selected near-term priority: exercise one Sol as the main working agent
on a medium-sized real task, with Luna subagent trees available and an explicit
brief to leverage Jev. Sol owns the outcome, integration, and judgment about
delegation. Luna children may recurse where useful; no tree shape or minimum
depth is prescribed. Additional Sol coordination levels are not the experiment.

The task should naturally involve investigation, implementation, checking, and
some independent obligations, with checks fast enough to support iteration.
Choose the actual task with the operator; this plan does not prescribe a new
feature or require the previous tags task to be repeated.

Teach the available Jev shapes through a few executable examples: select useful
evidence, classify a concrete condition, or prepare a worker's investigation.
Encourage substantial code-plus-Jev use where it helps, without a call quota or
requiring the agent to invent a controller framework. Give it useful defaults
and let the work reveal the next missing piece.

Include a worked multi-step program, not only isolated classifications: useful
parallel judgments over shared state, code acting on their answers, fresh
observations, then further semantic selection or a prepared model handoff.
The [leverage notes](jev-swarm-leverage.md) distinguish this from plumbing demos
and list other potentially strong patterns. Evidence preparation is a candidate
to explore, not an established winner or the only useful way to apply Jev.

### Run, interview, fix, rerun

1. Complete the medium-sized task through the actual tools. Observe crashes,
   authority errors, confusing types, missing names, noisy output, slow operations,
   and repeated manual recovery.
2. Ask where Jev helped, where the agent wanted to use it but could not, what
   it abandoned, and what Luna needed that the assignment omitted.
3. Fix the consequential friction before the next run. Save useful functions
   with examples; Fable can incorporate them into project source at restart.
4. Use the next run to exercise those fixes and reuse a promising component.
   A Luna child can be its second consumer; a new multi-Sol swarm is unnecessary.

The desired outcome is useful work completed without crashes or significant
interaction friction, with Jev serving actual work. A run that only succeeds
through repeated operator repairs exposes more work to do. This is practical
dogfood judgment, not proof that the runtime has no remaining bugs.

## Critical path now: programs can reference the agent’s own context

[Own-context access](jev-own-context.md) is a major part of reaching the Jev 10×
ambition. During the direct-use runs, investigate a Haskell read of the calling
Sol/Codex node’s recent conversation and native tool interactions. Combine that
snapshot with exact artifacts so reusable programs can use goals, corrections,
and previous attempts without the agent reconstructing them every time.

The live integration trial must be Codex-driven; Opus can prototype consumers
with fixtures. Proxy-submitted cell history is not the target agent’s conversation.
Start with the existing conversation owner and a bounded read, then demonstrate
that a real function benefits. Do not defer this until autonomous supervision.

## Critical path after that baseline is comfortable

First make direct use and Luna delegation work well. Then identify the repetitive
sequence the Sol actually performed and move that sequence into an authored
supervisor, adding Jev at its semantic branches. Prove one real completion and
repair cycle before expanding autonomous supervision or adding Sol levels.

Existing supervisor fixes and live proofs remain useful; a complete autonomous
controller is not a prerequisite for these next one or two runs. The shared
component experiment can proceed through ordinary source and manual adoption.

Increase concurrent obligations once local completion absorbs work rather than
multiplying parent interventions. Respect compiler/build/memory capacity, and
inspect whether the parent receives outcomes and genuine decisions rather than
more routine bookkeeping.

## Parallel path: improve ordinary tools immediately

This does not depend on completing the autonomous loop.

**First candidate: semantic tool-output filtering.** Take one noisy output family.
Retain the original output; let Jev select useful blocks for the initial response;
show that something was omitted and provide the existing output handle/offsets.
Preserve exit status and required facts. Verify expansion retrieves the hidden
material without rerunning the command. Ordinary output remains the fallback.

Use the agent’s recent context to make this view task-dependent. Once retrieval
works, deliberately explore aggressive selection: pagination and a discoverable
omission index make detail recoverable. Show what kinds of material were collapsed,
how much, and how to read it; do not hide its existence behind a generic truncation
marker. Full retention and retrieval lifetime must match what the view promises.

Treat the selection function as code the agents can improve. Save revealing misses,
revise its questions or composition, and reuse the revision. Expansion alone is not
a failure; an undiscoverable omission or missing evidence that changes a decision
is. Lack of expansion alone is not success. The
[detailed experiment](jev-swarm-leverage.md#aggressive-context-aware-views-with-a-revision-loop)
combines context access, pagination, and failure-driven component improvement.

Begin as an explicitly selected project helper if that is fastest. If callers
benefit but repeatedly forget to use it, integrate the behavior into the existing
tool-result owner. Do not add an extra model round to authorize every display
projection. The first implementation should be narrow enough to remove easily.

**Second candidate: prepare a repair request.** After a failed check, gather a
relevant source span and production usage or migration example. Give the worker
the original evidence with references. This can improve an otherwise unchanged
Luna task and later become part of the autonomous supervisor.

These experiments converge with the main path: the same evidence-selection
functions can serve direct tools, workers, and background actors.

## Publication and effect authoring can proceed alongside this

The README should explain the model and show short, actually executed examples:
agents write typed programs, combine exact operations with Jev judgments, and
keep useful improvements as ordinary project source. Shoal demonstrates the
possibility without defining its entire scope. Separate working behavior from
the 10× ambition and future live-sharing experiments.

An effect-authoring guide should follow one small real effect from its Haskell
surface through the current schema/handler owners to a focused execution check.
Link the existing owner documentation instead of creating another architecture
manual. Distinguish composing existing effects from adding a new host capability.

Neither publication nor the first experiments need a component registry,
automatic learning system, new GUI, cross-process closure transport, or polished
installation on every platform. Document the supported setup honestly.

## What to postpone until it removes observed friction

- Live callable exchange: try after source sharing exposes a useful same-run need.
- Semantic component discovery: nominate consumers manually until the collection
  is large enough that discovery is a real problem.
- Generic state-machine/graph abstractions: ordinary actor state and loops can
  carry the first investigations and deferred work.
- A universal memory or provenance framework: retain concrete cases and existing
  operation references; introduce structure at the consumer that needs it.
- Large evaluations: inspect natural failures, useful outcomes, actual spend,
  and consumer interviews first.

## The practical success criterion

The critical path is through **executed behavior, useful adoption, and a second
iteration**. A clever plan, a compiled actor, or a saved snippet is intermediate.
The next run should need less repeated reasoning because of what the previous
run taught us. Keep making that loop faster and the ambition becomes testable.
