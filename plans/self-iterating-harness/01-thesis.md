# The self-iterating harness — thesis & scope

## The idea

A harness whose behaviour is defined by **two pure Haskell functions** that
agents can iterate on. The running agent shapes its own context as it goes; a
smarter offline agent iterates on the harness *itself* between runs. Inspired
by Letta (agents editing their own context window) and by the "give an agent a
box and let it iterate on itself" research posture — but the thing being
iterated on is the **harness**, not model weights. We are not doing LLM dev.

The whole harness is these two functions plus a State type and some helpers:

```haskell
render :: State -> Maybe Text -> Text     -- build the system prompt from state
loop   :: State -> Harness State          -- run one context window of work
```

A runtime **alternates** them: `render` builds the prompt, `loop` runs a
context window's worth of agent work and returns the next `State`, repeat.

## The shape is a hylomorphism

The render/loop alternation is a hylo over the context window:

- `render` is the **coalgebra** — it *unfolds* the compact `State` into an
  expanded context window (the system prompt the agent works against).
- `loop` is the **algebra** — it *folds* the window's work back down into a new
  compact `State`.
- The context window is the **fused intermediate**: expanded into, folded out
  of, never durably retained. That is the whole point of a hylo.

This is the same hylo as exo's `fork_wave`/`merge` (unfold into subtree agents,
fold the branches back) — but **in-language**, as typed Haskell values and
functions rather than process-level worktree orchestration. Sub-agent
branching inside `loop` (via the `fork` effect, §03) is the tree version of the
same shape. "Haskell expands, Rust collapses; the language boundary is the hylo
boundary" (CLAUDE.md), lifted from compilation to the agent loop.

## The point: distillation

The payoff is not the runtime, it is the loop *around* it. A smarter offline
agent reads transcripts and crystallizes expertise into the harness Haskell —
conditionals in `render` ("given this State, remind the agent of X"),
sequencing/branch logic in `loop`. A cheaper agent running inside inherits all
of it for free, as deterministic reminders/prompts fired by State, without
paying for the smart agent's reasoning each turn.

**Pay for smartness once (offline), amortize it across many cheap runs.** The
harness gets smarter over time while the inner agent can get cheaper. The two
functions are the distillation medium. (Details in §05.)

## Scope of this spec

- **Design decisions + requirements. No implementation.** No Rust module
  layouts, no concrete function bodies. Type signatures that appear are the
  *proposed shape* of a decision, not a locked API, unless marked LOCKED.
- **Full scope**: both the runtime (§02–04) and the distillation loop (§05).
- Everything deferred or unresolved is collected honestly in §06. If a design
  point was not settled in the originating conversation, it is an open question
  here, not an invented answer.

## Relationship to `harness-r0` (do not wipe)

This supersedes the *framing* of `plans/harness-r0/` (typed interaction + a
Datastar observatory as the product) but **builds on and repurposes its
substrate**. `harness-r0` may still be in flight; it is not being retired here,
and `plans/README.md` is left pointing at it.

Reused from `harness-r0` / the shared substrate:

| harness-r0 piece | reused as |
|---|---|
| `returnControl @T` typed yield (GHC is the schema validator; an ill-typed answer does not consume the continuation; the GHC error is the retry prompt) | the `runLLMTurn` family (§03) |
| fork machinery (children run on the parent's suspended machine; session tree) | the `fork` effect (§03) |
| `Ui` eDSL + Datastar renderer | the initial Agent effect set (gui) (§03) |
| persistent session residency / stow engine | the resident session the runtime hosts (§02) |
| auth providers (ChatGPT OAuth + API key) | LLM-turn transport |

Built on **tidepool-repl** (the resident-session substrate shared with
`tidepool-harness`), which is the closest existing thing to what the runtime
does.
