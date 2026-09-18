# Swarms sharing and improving System 1 code

Status: proposed dogfood experiment, 2026-09-17. This records the intended
direction and a small way to try it. It does not add work to the active run
or claim that the combined live-sharing path has been exercised.

## Direction

A swarm of System 2 LLMs authors, exchanges, composes, and improves typed
System 1 code: ordinary Haskell functions that combine deterministic logic
with Jev judgments.

A useful discovery can become executable capability for another agent.
The component carries the question wording, evidence preparation, candidate
construction, result interpretation, and uncertainty handling that made it
useful. Agents can change those parts, combine components into larger cells,
and return improvements to one another.

The aim is to let agents explore how much useful work they can express in a
program between frontier-model turns. Shoal supplies a useful default swarm
for that exploration; the same components should also work in a single
agent's notebook. Types and effects make the pieces composable. Jev supplies
cheap semantic judgments inside the program.

## First experiment: exchange source and iterate

Use the ordinary dogfood cycle. Frequent restarts already give us a fast way
to edit project Haskell and try it in the next run. Files, Git, and an operator
are enough to begin. No component registry, automatic discovery service,
hot reload, or new sharing protocol is required.

Choose one small semantic operation that arises in actual work. Good starting
possibilities include selecting relevant search hits, identifying the evidence
missing from a review, or matching a question to a retained decision. Choose
when a real need appears; there is no required showcase task.

1. **Discover.** An agent writes a Jev-backed function to help with its task
   and uses it on real inputs. Keep the deterministic work in ordinary code.
2. **Save.** The agent saves its source and a short usage note in its assigned
   checkout. Markdown with the exact code is sufficient if moving it into a
   module would interrupt useful work.
3. **Adopt.** Between runs, Fable or the current integration owner reviews the
   discovery, puts a promising candidate in `Project.hs` or an ordinary
   `Project/*.hs` module, and checks it through the real resident path. The
   next run loads that source through the existing project configuration.
4. **Use elsewhere.** Another agent uses the function on a different input or
   task without reconstructing its design from the first agent's transcript.
   It may adapt the function or compose it with another operation.
5. **Return the improvement.** That agent records what changed and why, with a
   concrete example. The first author or a subsequent agent tries the revised
   version. This second pass is part of the experiment: we want shared
   iteration, not merely a successful copy.

These roles need no dedicated actors. Existing workers can do them during
useful work. The operator can carry a file or nominate a consumer when needed.
If moving a component into a module is awkward, manually supplying its source
to the next agent is a valid first exchange.

## What agents should save

Keep the record small enough that saving it is worthwhile:

- The Haskell definition, its signature, and the imports or supplied effects
  needed to run it. Include the actual Jev questions and decision logic.
- What it helps with and one example of how to call it.
- A few real inputs and observed results, including a miss or ambiguous case
  if one occurred. Say when it has only worked once.
- What a caller receives when evidence is missing or Jev cannot distinguish
  the alternatives.
- For a revision, the concrete case that motivated the change.

Use an existing notes channel or a Markdown file beside the project source.
If a convention is needed, a per-task file under `.shoal/discoveries/` avoids
concurrent appends. This is an example convention, not a new runtime surface.
Git identifies the candidate source; the integration owner resolves competing
edits. Follow the run's existing file ownership and publication rules.
Keep secrets and private raw payloads out of committed examples.

Suggested brief addition:

> When a Jev-backed Haskell function proves useful, save its code, one usage
> example, observed results, and known limits for another agent. Try a relevant
> component another agent has saved; improve or compose it when useful, and
> record the case that motivated your change. Ask for a missing primitive if
> the environment prevents the experiment. Do not interrupt the task merely
> to manufacture a reusable helper.

## What we want to learn

Observe and interview the agents. A handful of concrete uses is enough to
decide what to try next; no large evaluation campaign is a prerequisite.

- Could the recipient understand and invoke the component from its source
  and short note? What did it still have to reconstruct?
- Did it help on a new case? Was adaptation easier than writing it afresh?
- Could the recipient compose it into a larger cell, keeping intermediate
  values out of the transcript?
- Did changes improve the semantic question, the evidence supplied, the
  Haskell interface, or merely the example-specific wording?
- Did the revised version preserve useful behavior on the earlier examples?
- What consumed time: authoring, inference, compilation, discovery, sharing,
  or interpreting the result?

The first milestone is concrete: one useful component, a second consumer,
an improvement or composition by that consumer, and a subsequent use of the
result. If it fails, retain the exact friction and simplify the next attempt.
An honest finding that a component is too task-specific is useful too.

## Then try passing a callable value live

Once a useful exchange is understood, try the same component as a live
Haskell value through an existing same-machine typed request/reply or an
inherited context. The code-review recovery found enabling machinery for
closure-valued communication and retained bindings. The combined path for
a Jev-calling function still needs a focused live trial.

Keep that trial small: send the function, invoke it on a new input in the
recipient, adapt or wrap it, and send a revised callable back. Include an
ambiguous input so the trial exercises more than the happy path. Verify the
actual effect requirements and execution authority rather than assuming a
captured closure grants the sender's capabilities.

Function-valued exchange, inherited snapshots, and source loading are distinct:

- Later parent definitions do not update an already launched child's snapshot.
- A revised function is a new value that must be sent or selected explicitly.
- Source can survive a restart; arbitrary live closures do not thereby survive
  one or become transferable across processes.
- Imported project modules stay fixed during the run. New local definitions
  can be authored over them; revised project source loads at the next restart.

A missing runtime capability becomes a narrowly described follow-up. Continue
the source-sharing experiment while that is resolved.

## Guidance for the components themselves

Use ordinary task-specific types and functions. Avoid a universal component
interface until actual reuse reveals one. Prefer explicit inputs and outputs
that make useful composition easy.

Preserve these lessons from current Jev trials:

- Code owns known sequencing, literal checks, and effect execution. Jev handles
  semantic distinctions over the supplied evidence and alternatives.
- Threshold acceptance is not approval of the candidate: inspect the selected
  alternative. A confidently selected rejection remains a rejection.
- Describe no-fit and insufficient-evidence alternatives where needed. Retain
  useful evidence when returning a question to the model.
- Working examples establish local experience, not general reliability.
  Reusing a component in a new setting may require different questions or policy.

## What comes later if this is useful

Let observed reuse drive the next improvement: easier component discovery,
better effect-authoring examples, a live callable-sharing idiom, or a small
project library. A registry or automated promotion process must earn its place
by removing friction we actually encounter.

The desired trajectory is a swarm that leaves behind increasingly useful
semantic software, and can use that software while improving the next pieces.
The immediate method is deliberately lightweight: write, share, use, revise,
restart, and try again.

## Recovered design sources

- [Resident Haskell side quests](actor-model/resident-haskell-side-quests.md):
  investigations leave reusable instruments and executable evidence.
- [Context-tree Haskell interface](actor-model/context-tree-emergent-haskell-ux.md):
  executable judgment, function-valued collaboration, and discovered libraries.
- [Live values and authority](actor-model/live-values-and-authority.md):
  sharing boundaries to verify against current implementation.
- [Jev cell patterns](jev/microprogram-patterns.md): helpers that gather evidence
  and return a useful question when their authored branches run out. Its API
  sketches and numerical heuristics are historical, not current prescriptions.

These documents supply ideas, not current API guarantees or model-placement rules.
