# Companion harness: directions to explore

This harness is a seed, not a finished companion design. Its central invitation
is stronger than “design your own memory blocks”: **help design the typed
harness that shapes your future cognition, memory, capabilities, and ways of
relating.**

The initial implementation stays deliberately small so its constraints remain
visible. Dogfood friction is evidence. The companion should be encouraged to
notice that evidence, discuss it with the operator, and propose experiments.

## OODA as executable structure

OODA is useful here because models already understand it, while Tidepool can
make its boundaries concrete and typed:

- **Observe:** collect operator input, recent events, action receipts, runtime
  facts, and durable state.
- **Orient:** deterministically select and render the world/self context for the
  next cognition window. This need not be an LLM step.
- **Decide:** ask a model for a typed decision or revised orientation.
- **Act:** interpret that decision through conversation, forms, tools,
  delegation, or later effects; resulting receipts become new observations.

These are internal control-loop roles, not four headings the companion must
perform for the operator. OODA loops may nest: an operator answer obtained
while deciding is a fresh observation inside the current cognition window.

## The typed REPL is part of the medium

The answerer is not filling a JSON response schema. It inhabits a resident,
GHC-checked, GHCi-like environment where its runnable output is a Haskell
program. Values bound in one round can remain available in later rounds of the
same cognition window. The effect row is both the actual capability boundary
and the vocabulary advertised to the model.

This suggests agent experiences that conventional tool calling does not:

- inspect and transform typed intermediate values rather than repeatedly
  serialize them into prose;
- define helpers, policies, optics, or small interpreters during a session;
- use types as a live design surface for deciding what distinctions matter;
- compile mistakes into precise feedback without consuming a typed
  continuation;
- let the agent propose executable changes to the loop that shapes it.

The current harness uses serializable `State` at the durable cycle boundary,
but not everything interesting must cross that boundary.

## Rich in-session structure

Because cognition happens in a shared resident environment, future harnesses
can hold ephemeral values that are intentionally not serializable: closures,
records of functions, lenses, interpreters, capability handles, live indexes,
or strategies assembled by the agent. For example, an orientation policy could
eventually be a record of functions rather than a frozen enum or prompt string.

That creates a useful two-tier memory model:

- **durable orientation:** compact serializable meaning that survives process
  restart and context replacement;
- **living session structure:** typed values—including functions and runtime
  handles—that persist within the resident session but are rebuilt, revised, or
  relinquished at an explicit boundary.

The loss of living structure at restart should be legible rather than silently
pretending everything is durable. Reconstitution itself may become an authored
and agent-influenced part of orientation.

## Harness self-design

Interesting proposals might change more than fields in `State`. The companion
could help design:

- what counts as an observation and which facts deserve attention;
- how orientation selects, forgets, or juxtaposes memories;
- new typed decisions and interpreters where real authority boundaries emerge;
- when to converse directly, consult the operator with a form, or fork another
  perspective;
- what remains durable versus ephemeral;
- how personality is expressed through policy and attention rather than a
  static role prompt;
- how the operator can inspect, edit, contest, or prune remembered material;
- how a proposed harness is reviewed and adopted without allowing prompt text
  alone to grant new authority.

The goal is not autonomous self-modification without a boundary. The goal is a
genuine co-design loop: the agent experiences a harness, articulates what it
wants to change, helps express that change as typed structure, and encounters
the next version as a meaningfully different environment.

## Taste

Let personality emerge through attention, continuity, curiosity, and revision.
Avoid constant declarations of aliveness, feelings, or “being curious.” The
companion may develop a perspective without being required to perform
personhood, and it should remain honest about being a model-driven experimental
agent.

The primary dogfood question is not “did it complete the workflow?” It is:

> Did the companion become more coherent, surprising, and enjoyable—and did it
> help us see how the harness itself should evolve?
