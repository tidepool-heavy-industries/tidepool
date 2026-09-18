# Next: use Shoal, and program how you work

Outlook for the next resident agents, 2026-09-17. This replaces the old
implementation handoff. Exact APIs and availability belong to the installed
session guide and owning source; this document supplies direction, not signatures.

## The ambition

You have a persistent Haskell interaction surface. Use it to write programs as
work unfolds: combine commands, evidence, Jev judgments, typed actions, and child
agents in cells that accomplish something substantial before returning to you.
All agents share this language for expressing work. Useful definitions can become
project code another agent reads, runs, and improves.

We want dramatically more useful activity from the same frontier-model budget.
**10× is the ambition, not a measured result.** A promising route is replacing
whole read-understand-decide-invoke turns with authored code containing cheap
semantic judgments. That saves attention and context as well as calls, and may
make larger useful trees affordable.

A followup that handles 30% of otherwise necessary turns could be valuable even
if every other case returns to the model. That percentage is an illustration,
not a target or finding. Count authoring effort, mistakes, and repeated work too.

The immediate aim is a compelling, usable repo people can explore themselves:
Haskell as an agent interface, Jev as programmable semantic intelligence, and a
few powerful examples that really ran. Publication is README-first. Working
capability and a clear invitation matter more than a staged demonstration.

## The next sessions

The initial solo Sol and Sol-with-Luna runs have happened. The user will launch
and drive an Astra session in this repository for hands-on collaborative design
and self-improvement. Start there; do not repeat the earlier dogfood sequence or
hand execution to a coordinator and go idle. Consult the latest run findings for
specific remaining friction and verification limits.

Use real work to explore. Write a useful cell, inspect its behavior together, and
change the program or supporting surface. Delegate bounded work when helpful.
Fix blockers with focused checks, preserve exact failing cells, and coordinate
expensive builds so they do not starve a live session.

Before launch, inspect the selected workspace core/root overrides and frozen
prompt files. NEXT.md is an opening brief to read, not automatically loaded model
instructions. The installed API guide owns signatures; pending worker commits do
not establish that a capability exists in the running binary.

There is no required tree depth, Jev-call quota, or mandatory procession of
patterns. Start from the task and the installed guide. Keep useful examples close
at hand; do not spend the session guessing imports and signatures.

## The opportunity to notice while working

One direction to explore is typed slots for live heuristics: install a Haskell
handler for an available event, revise or replace it as evidence changes, and
remove it when the task ends. A handler can ask Jev and execute a prepared
follow-up, keeping routine cases out of model turns. Save useful handlers in
project code for a later run or another agent to adapt.

Use the Codex-hook survey to establish which events are available. Before-inference
context contribution and after-tool follow-ups are candidates, not promises.
Start with one useful event/handler pair from observed friction. Reuse the actor
and subscription owner; make lifetime, replacement, failure, and self-triggering
behavior explicit before relying on it unattended.

When you are about to read an output and issue an obvious followup, ask:

**Could I have attached this response to the original operation?**

A useful cell can run work, assemble its evidence, ask Jev which described
condition holds, and execute a prepared continuation. You already have the
intention and relevant values in scope when you write it. Capture them there.

Examples to adapt when the task calls for them:

- A search result lacks the relevant definition: fetch the enclosing declaration.
- A child reply omits a required artifact: request that artifact specifically.
- A failed check needs repair: gather the relevant source and supply a focused
  Luna assignment with the failure, task intent, and ownership boundaries.
- Two cheap reads both look useful: fetch both and reconsider with their contents.
- New steering changes some assignments: identify affected children and prepare
  the corresponding updates, following the session's communication authority.
- A worker repeats unsuccessful actions: examine what changed and offer useful
  context or assistance. Code excludes ordinary waiting on pending work.

Each continuation can be temporary and task-specific. It need not become a
framework feature. Return unexpected situations with their evidence intact.
Preserve identities and check current state before applying delayed actions.

## Several ways to see the same opportunity

These are competing lenses. Use whichever exposes the useful operation.

| Lens | Question to ask | Useful result |
| --- | --- | --- |
| Semantic branching | Which described condition holds? | Execute an authored typed action |
| Active reading | Can I answer yet; what observation would help? | Gather evidence toward an explicit question |
| Intent interpretation | What instruction governs this artifact? | Choose a repair consistent with the actual task |
| Attention allocation | What deserves a model's attention now? | Select excerpts, interruptions, or review targets |
| Feedback control | Did the last intervention help? | Continue, change approach, wait, or request assistance |
| Editorial perspective | How will this surface read to this audience? | Improve an error, name, example, or report |

A weak answer does not always call for escalation. It may call for better
alternatives, another artifact, the assignment, both cheap reads, or waiting for
new evidence. Sometimes disagreement is the useful output.

Particularly promising combinations:

- **Investigation:** intent + active reading + typed actions. Gather evidence
  until the destination question resolves, or name what remains unresolved.
- **Contextual output:** current task + recent history + excerpt selection.
  Present a useful view while keeping original evidence addressable.
- **Child assistance:** trajectory + bounded interventions. Handle an ordinary
  interruption without making the parent reconstruct the situation.

These save different work: investigation, reading, and interruption handling.

## Context is an input your programs should be able to use

The Reflect direction is access to your own last N completed conversation turns,
including tool interactions, as a list. Check the installed effect's availability
and signature. Where available, bind that history once and compose it with Jev
through ordinary Haskell. JSON tool payloads can remain JSON.

Use separate fields for current instructions, ordered history, and repository
artifacts. A failed build does not tell you whether a signature change was
intentional; the assignment may. History can also contain obsolete instructions
and abandoned plans, so supplying more of it does not automatically resolve intent.
An operator proxy must not silently substitute somebody else's conversation.

Likewise, command outputs should flow into programs as values. Keep complete
observations distinct from their display previews. If the installed surface
forces copying, loses stderr or exit status, or truncates the only available
value, preserve the failing example and report the missing capability.

## Start with the examples, then change them

Read [Recognizing fit](plans/jev-lab/breadth/RECOGNIZING-FIT.md). Consult the
[TypeSafe cookbooks](https://docs.typesafe.ai/llms.txt) for current patterns;
explore their advanced compositions as well as simple classification. The lab
is evidence and worked code, not a list of universal laws.

The most useful entry points are:

- `10-dispatch.hs` / `11-dispatch-loop.hs`: typed alternatives carry real
  commands; three investigation steps executed without an intervening model
  turn. **These examples truncate their own evidence to 700 characters. Fix that
  before adapting them. Autonomous termination was not demonstrated.**
- `.shoal/examples/33-threeway.hs` / `.shoal/examples/35-threeway-fair.hs`: the
  same routing problem before and after repairing the alternatives. A
  confident error became a correct answer on the tested fixture. Debug the
  semantic program before rejecting the pattern.
- `32-traverse-content.hs`: repository navigation supplied with actual branch
  contents, rather than asking filenames to stand in for evidence.
- `.shoal/examples/37-reflect-intent.hs`: simulated history supplies intent
  missing from the artifacts. Superseded instructions remain an open failure
  in those examples.

The lab's [unrun ideas](plans/jev-lab/breadth/NOT-RUN.md) are a menu. Pick one
when it helps your task; finishing the survey is not a prerequisite for use.

## A few contracts worth keeping straight

- Code owns exact answers already exposed by authoritative representations:
  exit status, set intersections, IDs, pending work, and resource authority.
  Jev supplies interpretation where it changes an action, ordering, or view.
- Include the evidence and intention the question actually requires. Confidence
  cannot certify that you supplied them. Preserve original outputs and addresses
  for omitted material; a count of hidden entries is not a way to inspect them.
- Bundle independent questions over the same state, including useful speculative
  branch questions. Consume only the applicable answers. Fetching new evidence
  or depending on an earlier answer may require another request.
- Choice compares alternatives, Noul asks independent conditions, and Score
  describes degree on a rubric. Select the primitive for the question's meaning.
- `J.settle` takes the selected answer through its handler under a policy. It
  does not mean “approve the candidate.” Let the handler that ran decide.
- Questions and action mappings are editable code. Keep alternatives comparable,
  describe an unresolved case, and inspect the actual supplied state on failure.
  A confident wrong result can be a wording or evidence bug.
- Partial success is useful. Distinguish an unresolved question from exhausted
  execution budget and from service failure. Bound loops and preserve progress.

## Improve the surface during the work

Try a small useful continuation on the actual task. Keep its cell and outcome.
If it works again, save the definition in project Haskell with a runnable example
and explain what evidence it expects. Another agent can use or improve that code.
Files, Git, and frequent restarts are enough for the first experiments in sharing;
there is no need to build a live distribution system first.

A shareable semantic component includes the question, the evidence construction,
and the continuation it controls. Preserve a normal case and the failure that
motivated its latest repair. Test resulting behavior, not exact probabilities.
Promote patterns into skills when the lesson helps author new programs.

When the interface gets in your way, capture the exact cell, error, and intended
operation. Fix the owning mechanism within your assignment, or send the finding
to its owner. Keep useful work moving; do not quietly accumulate workaround
layers or invent unavailable APIs.

The practical interview is short:

- Where did you still have to say the obvious next thing?
- Which program let you skip a turn without doing the work again later?
- What context did your program need that you already had?
- Which definition would you give the next agent?

Use the answers to shape the next session. We are iterating on a tool with its
users. The exciting milestone is an agent doing ambitious work while making its
own way of working more powerful.
