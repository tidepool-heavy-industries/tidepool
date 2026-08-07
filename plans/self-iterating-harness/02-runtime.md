# Runtime model — render/loop, monads, State, compaction

## The top-level loop

The runtime alternates `render` and `loop` indefinitely, threading `State`:

```
state, lastCompaction := initial, Nothing
forever:
    prompt        := render(state, lastCompaction)     -- build the window
    state, comp   := executeLoop(prompt, state)         -- run one window
    lastCompaction := comp                              -- Maybe Text, feeds next render
```

- `State` is **serialized between loops** (persisted, survives restart).
- The runtime **restarts** to pick up a new harness version. There is **no live
  hot-reload** in v1 (live mid-session resume is a neat later option, §06).
- One `loop` invocation = one context window's worth of work, ending at a
  compaction boundary. The runtime's *next* call is where the fresh `render`
  (and the cache refresh) happens.

## Two monads over one resident heap

The runtime hosts a resident (tidepool-repl-style) session. Both monads execute
in that one session and share its heap.

- **`Harness`** — the LLM-orchestration monad. Effects: `runLLMTurn` /
  `runLLMTurnWithRequiredResp`, `fork`, and whatever high-level orchestration we
  need (added over time). This is where "run agent turns for me, handle type
  retries" lives. `Harness` does **not** expose a State effect — State is a
  plain value threaded through `loop`'s signature.
- **`Agent`** — the effect stack the inner agent's repl snippets run in (§03).

**In-heap, nonserialized value passing *within* a loop.** Because `Harness` and
`Agent` share one heap, values crossing between them (e.g. the result of an
agent turn) need not serialize — they can be functions, closures, effectful
actions, any Haskell value. **State is serialized *across* loops.** Two
channels: the heap is the fast/expressive within-loop channel; the serialized
State is the slow/durable across-loop channel. Do not blur them.

## State

- `State` is any **author-defined** type `s` with `(ToJSON s, FromJSON s)`
  instances. These are **typeclass constraints** (bounds on `s`), used by the
  *runtime* to persist State across loops — **not** an effect in either stack.
  LOCKED.
- This is the "bag of typed values": small, typed — enums, integer levels
  (e.g. user-energy, mode), tag lists, per-loop notes-as-strings, etc. Whether
  it accumulates a history of past-loop summaries (for decayed rendering) is an
  author choice; State is author-defined.
- Threaded purely through `loop :: State -> Harness State`; read by
  `render :: State -> Maybe Text -> Text`. No effectful State read/write. LOCKED.

## render

- `render :: State -> Maybe Text -> Text`. LOCKED signature.
- The `Maybe Text` is the **previous loop's compaction text**; `Nothing` only on
  the first loop (before any compaction exists).
- **Runtime-invoked at loop boundaries only — never per-turn.** The harness
  author cannot call `render` mid-loop. This makes per-turn system-prompt
  re-rendering (and thus per-turn cache-blowing) **unrepresentable by
  construction** — a footgun the DSL removes rather than a discipline it asks
  for.
- Rendering uses Haskell's `[fmt|]` quasiquoter + plain Haskell conditionals.
  **No jinja.** Conditional memory blocks are ordinary `if`/`case` over `State`
  — typed and testable.

## Compaction

- **Structural (normal).** The loop's own work produces the next `State`; that
  State update *is* the durable, typed memory carried forward. Compaction is not
  a separate task — it is the designed terminus of a loop.
- **Text compaction.** A "compact to text, target X tokens" turn emits `Text`
  — a prose summary of the just-ended window — which becomes the next `render`'s
  `Maybe Text` input. It fires either as the loop's designed final turn, or is
  force-triggered by the runtime (below).
- **Emergency trigger — runtime-owned.** The runtime watches context usage and,
  at a configurable threshold (~80%), forces the "compact to text" turn and ends
  the loop early with the current `State`. LOCKED: the *runtime* owns the
  trigger, not the loop — the loop cannot be trusted to watch its own budget
  mid-flow, the same reason it cannot own `render`. Structural compaction alone
  is unguarded; the emergency trigger is the backstop.

## Cache economics

The frozen prefix (the system prompt from `render`) changes only at loop
boundaries. Within a loop, per-turn variation — which effects are available,
the required output shape of a turn — rides the **user-message lane**, never the
cached prefix. So the prefix stays cache-warm across all of a loop's turns, and
the only prefix change is the (already cache-invalidating) boundary re-render.
