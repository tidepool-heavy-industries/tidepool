# Let authored Haskell use the agent’s own working context

Status: proposed critical-path experiment for the Jev 10× ambition, 2026-09-17.
Not implemented or verified by this planning pass.

## Why this is central

An agent should be able to reference its own recent conversation from an authored
program. Today our experiments tend to choose between a manually reconstructed
context packet and isolated artifacts. A third option is to combine exact evidence
with the goals, corrections, failed approaches, and tool interactions already in
the agent’s working context.

This can remove repeated explanation from expensive model turns and let reusable
System 1 functions adapt to the task they are called from. The function knows not
only what a diagnostic says, but what the agent is trying to accomplish and what
has already been attempted. This is a major part of the
[10× strategy](jev-swarm-leverage.md), not merely a convenience for logging.

## First surface to investigate

Expose a small Haskell read through the existing conversation owner: a bounded
snapshot of the calling agent’s recent completed turns, including available native
tool calls and results, rendered as role-labeled text. No API name is prescribed
here; inspect existing owners and consumers before choosing one.

The source must be the Sol/Codex node’s conversation. A proxy operator’s submitted
Haskell cells are a different history and cannot stand in for it. The initial real
integration experiment belongs in a Codex-driven session because that is where the
relevant integration exists. Opus can develop the Haskell consumer with explicit
text fixtures, but that does not prove live context retrieval.

Start with the agent-visible transcript, not hidden reasoning or an assertion that
we can reproduce the exact model input. Establish what the backend actually makes
available, especially after compaction, before promising completeness.

## Composition

A useful authored program assembles:

- Explicit current task and candidate identity.
- Concrete evidence: source, diagnostics, command results, and their provenance.
- A separate recent-context field containing conversation and native tool history.
- Questions and typed actions appropriate to that program.

An authored wrapper can include context on every invocation within an investigation.
Make inclusion easy and selectable; avoid silently attaching history to every Jev
call globally. Different questions need different slices, and repeated context has
real token and latency costs even when Jev is cheap.

Conversation supplies intent and history; it does not make an old claim into a
current artifact or override runtime authority. Preserve speaker/tool labels and
order so corrections and superseded attempts remain distinguishable.

## Smallest useful experiment

1. In a Codex-driven Shoal node, find the existing conversation owner and the
   smallest supported route for reading recent completed turns. Extend that owner
   if needed; do not introduce a second transcript registry or memory system.
2. Choose a real investigation where an earlier user correction or failed approach
   matters and is absent from the artifact packet. Adapt `look` or another useful
   project function to consume the context as a separate input.
3. Run the same useful question with the artifact packet alone and with the recent
   context. Inspect whether the answer uses the relevant correction, avoids repeating
   a failed approach, or chooses a better next read. This is an exploratory comparison,
   not a statistical campaign.
4. Try a case with stale or irrelevant conversation. Inspect whether exact evidence
   and the current task still control the result. Adjust the slice or wording if needed.
5. Let the agent call the function during ordinary work without manually restating
   its history. Save the reusable function and the revealing examples.

Success is a useful program needing less agent-authored context while making better
use of information already present. Retrieving and echoing a transcript alone does
not demonstrate the benefit.

## Details the owning implementation must settle

- Define a turn and a snapshot boundary. Default to completed turns before the
  executing cell; avoid recursively including that cell’s expanding output.
- Bound both turns and bytes. Represent truncation and missing history explicitly;
  distinguish an empty conversation from an unavailable read.
- Identify the actor and snapshot. Retain existing actor visibility rules; a proxy’s
  authority does not make its own transcript interchangeable with the target’s.
- Preserve native tool outcomes where available. Large outputs can retain references
  to existing output storage rather than being copied in full on every call.
- Understand compaction semantics and report incomplete evidence. Do not claim a
  recent transcript equals the full normalized provider request.
- Read through existing owners and transport boundaries. Do not scrape terminal panes
  or add a parallel durable history as the default architecture.

## First major application: improve what the agent initially sees

Context access makes Jev-driven truncation sensitive to the actual task and recent
attempts. The same command output can foreground regression evidence, repair sites,
or an unfamiliar subsystem depending on what the agent is doing.

Retain complete source output, render selected original blocks, and expose a compact
index of omitted material with existing pagination references. This permits
aggressive initial views while keeping recovery possible and discoverable. Keep
exit status and other required result facts explicit.

Agents can then revise their own selection functions from actual use: which detail
was worth expanding, which omission was misleading, and which missing evidence
would have changed the decision. Start with saved examples and ordinary project
source revisions. See the
[selection-and-revision experiment](jev-swarm-leverage.md#aggressive-context-aware-views-with-a-revision-loop).

## How this combines with the other experiments

- **Semantic investigation:** use recent attempts to select genuinely new evidence.
- **Tool-output views:** judge relevance against what the agent is currently doing.
- **Typed action selection:** include intent and corrections alongside observed state.
- **Shared System 1 functions:** reuse a function without rewriting its contextual
  brief on every invocation; allow explicit context arguments for tests and replay.
- **Failure-driven improvement:** save the relevant snapshot with a revealing failure
  so later revisions can explain why the function’s decision was unhelpful.

Begin this integration during the next direct-use runs. It need not wait for an
autonomous supervisor, recursive swarm, or generalized memory design.
