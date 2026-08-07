# The distillation loop — smart agents crystallize into the harness

## Two loops, two speeds

- **Fast (within a session).** The running (possibly cheaper) agent operates
  through render/loop, its `State` updating at loop end (per-stage recording is
  also possible). This is the runtime of §02–03.
- **Slow (between sessions).** A **smarter offline agent** — claude-code /
  codex, **human-steered** — reads transcripts and freeform friction notes and
  edits the harness Haskell repo (§04).

## Distillation

The slow-loop agent **crystallizes expertise into the two functions**:
conditionals in `render` ("given this State, remind the agent of X"),
sequencing/branch logic in `loop`. The inner agent inherits these as
deterministic reminders/prompts fired by State, without paying for the smart
agent's reasoning each turn.

**Economic point:** pay for smartness once (offline), amortize across many cheap
runs. The harness gets smarter over time while the inner agent can get cheaper.
This is the delegate-to-small-models pattern with the harness Haskell as the
crystallization substrate: the smart model distills, the small model executes
inside the distilled harness.

## The friction channel

The running agent can emit **freeform prose notes** — or just an inline comment
in a repl snippet — reporting harness friction/improvement ideas. This is an
input to the slow loop, *alongside* transcript reading (not instead of it). It
is deliberately freeform; no structured report type in v1.

## Fitness signal — DEFERRED

How a harness edit is judged good is a **TODO**, deliberately deferred:

- At scale, real metrics (turns, tokens, task success, A/B branches) become
  possible.
- Not at scale, **human guidance/taste is the fitness signal** — and that is
  fine for now.

**Honest bound:** without an automatic fitness signal, a heuristic distilled
from a few transcripts can overfit noise and make the inner agent *worse*, and
only human taste catches it. This is the thing that eventually decides whether
the harness converges toward expertise or accretes cargo-cult reminders. It is
not solved here; it is named.

## Roles & recursion depth

For the foreseeable future: **distinct roles** — task agents (running inside a
harness) versus the improver (claude-code, outside, editing the repo). The
improver improving *its own* harness (turtles-all-the-way-down) is a possible
later direction, **not v1**.

## Out of scope

- **Gaming / safety of self-modification** — handled at the LLM-provider layer;
  we are not scaling this to a many-agent adversarial regime, so it is out of
  scope here.
- Auto-eval, multi-branch A/B promotion, and metric-driven selection — deferred
  with the fitness signal above.
