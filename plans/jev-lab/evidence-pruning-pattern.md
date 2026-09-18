# Run it noisy, then prune by relevance

A pattern for tools and widgets, from the Astra flight of 2026-09-17/18 and the
design conversation that followed. Written down because it inverts a discipline
we have been teaching, and because several planned pieces of work exist to serve
it.

## The shape

    condition holds
      → run a deliberately NOISY command
      → score each line or hunk for relevance
          against the caller's last N turns plus the explicit task
      → return the kept evidence, its scores, what was dropped, and a handle
        to the whole thing

Concretely: a check tool that, when the check fails, attaches the repository diff
— but the diff arrives already ranked and cut against what the caller has
actually been doing, with the full diff still retrievable.

The conditional part matters as much as the pruning. `add if X` keeps the
enrichment off the common path: a passing check does not pay for a diff nobody
will read.

## Why it is worth naming

We have been teaching the opposite discipline, and the flight shows what that
costs. The workbench guidance tells a model to bound its searches by hand: use a
pathspec, limit with head or tail, filter to diagnostic lines, and never truncate
from the front because a check log opens with build preamble and closes with the
errors. Every one of those is the model hand-rolling a relevance heuristic out of
line numbers, because line numbers were the only tool it had.

`Cmd.quiet` exists for the same reason. One cell buried a single number under
7,247 bytes, and the remedy we shipped was to silence the command. Silencing is a
blunt instrument: it throws away the evidence along with the noise.

Ranking is the better answer to the same problem. If pruning is cheap and
semantic, a model can stop crafting narrow commands and run the obvious wide one.
The flight's own `contextualViews` was an attempt at this and its notes record the
limit it hit: a neighbour-distance rule "preserved extra routine compilation text
as well as diagnostic context". A neighbour rule is a proxy for relevance. This
pattern asks the question directly instead.

## Why the last N turns belong in the context

The same noisy output should prune differently depending on what the caller is
doing. Debugging a failure wants the failing implementation. Reviewing a
migration wants the affected callers. Chasing environment trouble wants the
toolchain evidence. Without the recent turns, the caller must restate the task on
every invocation, which is the tax the pattern exists to remove.

The authority boundary is already correct and enforced rather than conventional:
the conversation reader is called with the executing actor and nothing else
(`tidepool-actor/src/resident_actor.rs:2074-2094`), so a tool reads its own
actor's turns and cannot reach another's. Reading turns is a distinct
`ActorEffectKey::Reflect` (`tidepool-actor/src/role.rs:79`), so a tool that wants
it must hold it. A background actor therefore gets an intentional snapshot rather
than silently borrowing a conversation.

## The guards

These are what keep it from becoming a machine that hides evidence.

1. **A pruned view must never look like the whole.** This is the flight's own rule
   for stream capture — *"A convenient call must not turn incomplete capture into
   apparently complete strings"* — applied to ranking. The result type must say it
   is a selection.
2. **The full evidence stays addressable.** Pruning returns a handle, not a
   replacement. Recovering what was cut must not re-run the command.
3. **The prune says what it dropped and on what grounds.** A dropped hunk with its
   score is recoverable evidence about the prune itself; a silently dropped hunk
   is not.
4. **Scores are data, not prose.** A consumer decides what to do with a low score;
   the widget does not decide for it. The threshold is a policy the caller passes,
   never a universal constant baked in.
5. **Observations stay distinct from judgments.** The spans, the diff and the exit
   status are observations. The relevance score is a judgment. A return value that
   blends them cannot be audited.
6. **The prune is itself traced.** Which turns were read, what was kept, what was
   cut. Without that record a tool sensitive to context is indistinguishable from
   a tool behaving at random, and cannot be revised later.

## What it needs from work already planned

| Needs | Where it comes from |
|---|---|
| Ranking structure, not strings | structured diagnostics: `ExtractDiagnostic` (span, severity, message) already exists in `tidepool-extract-report`; it is flattened before any program sees it |
| Reading recent turns inside a tool | works today, authority-gated; made usable by the observation fix, since a large history is exactly what used to fail to bind |
| Returning large pruned evidence with a handle | the retained-handle bind, plus the receipt-as-reference rule |
| Recording what was dropped | the tracing work's content target |
| Shipping the widget and revising it | project source reload, plus compiled tool records refreshing with it |
| Composing several prunes into one packet | the jev-dsl switch: a packet is a value, so a widget can build questions without making a call |

Every row is already a planned item. None of them was planned for this. That is
the argument for the pattern: it is what several separate pieces of work are
jointly for.

## Sketch

Shape only, not an API proposal. The vocabulary is the new jev-dsl surface, where
a battery carries its rows so an answer arrives beside the thing it judged.

    checkWithContext task = do
      outcome <- runCheck task
      case outcome of
        Passed -> pure (plain outcome)          -- add if X: nothing to add
        Failed -> do
          diff    <- wideDiff                    -- deliberately noisy
          recent  <- reflect n                   -- the caller's own turns
          ranked  <- rank recent task diff       -- one battery, rows carried
          pure (pruned outcome ranked (handleFor diff))

`rank` builds its questions without making the call where the caller might want to
fuse it with another prune into one packet; it owns its call where it must acquire
evidence between stages. Making every helper secretly perform its own request
would lose most of the composition advantage.

## Open questions

Put to Astra, not yet answered:

- Is relevance one judgment or several? Asking whether a hunk is causal, merely
  adjacent, or contradicts the stated task may beat a single relevance score.
- When a prune drops something that was needed, should that surface at the moment
  of the drop or when the conclusion later turns out wrong?
- Should a prune ever refuse — reporting that the evidence resists reduction
  rather than returning a confident selection?
