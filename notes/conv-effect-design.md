# Conv: threading minimal intelligence through an effect

Status: **design, not built.** Captures a converged brainstorm. v1 scope is
deliberately tiny; later phases have seams reserved but no code.

## Motivation

Today `llm` is a stateless, amnesiac oracle: prompt in, schema-validated JSON
out, nothing remembered. You can't *hold* a conversation, only fire one-shots.

The idea: an LLM call should be a **resident object you can grow**, so the gap
between "one-shot classification" and "ad-hoc agentic loop" is just *how many
times you poke the same object* — not two different APIs. When the model running
the harness (e.g. Claude via MCP) hits a complex effect, it can thread a little
LLM cognition through it: classify, then maybe loop, then maybe call tools, then
maybe ask a human — all on one continuum.

Framing that disciplines every decision below: a `Conv` is a **session-scoped,
semi-ephemeral, "thread minimal intelligence through this" primitive** — not a
durable long-lived agent.

## The split

**Rust owns the conversation as a resident object with dumb verbs. The
intelligence of the loop stays in Haskell.** Tool execution never crosses to
Rust — a tool call comes *back* as data; Haskell dispatches it (into
`lsp`/`fs`/`exec`/`ask`/even a nested `Conv`) and feeds the result back in. So
"thread an LLM through an effect" is literally recursive: an agentic sub-loop is
just a tool whose body runs another `Conv`.

Because tool handlers are `M` closures, the loop *must* be Haskell-side (closures
don't cross the FFI boundary). That's what keeps Rust dumb.

## User-facing surface (v1)

The only thing the eval author writes is a **tool table** and a call:

```haskell
data Tool = Tool
  { tName :: Text
  , tArgs :: Schema            -- arg schema the model sees (existing Schema ADT)
  , tDesc :: Text
  , tRun  :: Value -> M Value  -- handler: ANY effects — lsp/fs/exec/ask/nested converse
  }

-- the model -> tool -> model loop runs INSIDE; returns on final answer
converse :: Text -> [Tool] -> Text -> M Value
--          system    tools     user-msg
```

The whole one-shot -> agentic spectrum on three primitives:

```haskell
-- one-shot classify: no tools, model answers immediately
classify sys x = converse sys [] x

-- agentic loop: hand it a toolbox, it loops until it stops calling tools
fixBug = converse agentSys [readFileTool, editTool, runTestsTool, askHumanTool] goal
```

`ask` is just a row in the table — a tool whose `tRun` is `ask schema prompt`.
So **autonomous and human-in-the-loop are the same machinery, different rows.**
When that tool fires, `ask` suspends the *entire loop* through the existing Ask
park (worker blocks on `response_rx`); on resume the loop continues mid-
conversation. This composes because the `Conv` is session-resident and outlives
any single `send`/`resume` — it survives the suspension untouched.

## Internal protocol (not exposed in v1, but built as real functions)

Surface directive: **high-level only now, but structured so the low level is
additive — a visibility change, not a rewrite.** So these are real internal
functions with final signatures; `converse` is a thin wrapper.

Rust verbs (thin, dumb):

```
create :: Text -> [ToolSpec] -> M Conv     -- system prompt + tool schemas -> resident id
send   :: Conv -> Text  -> M Turn          -- append user turn, run model, advance
resume :: Conv -> [Value] -> M Turn         -- feed tool results, advance
fork   :: Conv -> M Conv                     -- new id, COW share of history-so-far
```

Internal response type (flat, no exposed closure — the helper holds the `Conv`):

```haskell
data Turn
  = Reply  Value                 -- assistant produced a final answer (no tool calls)
  | Invoke [(Text, Value)]       -- model wants these tools (batch / parallel calls)
```

`converse` is the fold over it:

```haskell
converse sys tools userMsg = do
  c <- create sys (map toSpec tools)
  let tbl = [(tName t, tRun t) | t <- tools]
      go (Reply v)      = pure v
      go (Invoke calls) = do
        results <- traverse (dispatch tbl) calls   -- serial: M is single-threaded
        go =<< resume c results
  go =<< send c userMsg
```

Batch `Invoke` (list of calls) supports the parallel tool calls modern models
emit in one turn; handlers run serially (single worker), results gathered, one
`resume`. **Turn shape marked TODO** — leading candidate is exactly this
machinery-driven table; revisit only if a power-user driver needs the explicit
`(Value -> M Turn)` continuation form.

## Tool errors: feed back, model retries

A Haskell exception or `Left` from `tRun` becomes a `tool_result(is_error=true)`
the model sees, so it can self-correct and retry. The loop survives a flaky tool
— standard agentic resilience. (The error is caught at the dispatch boundary,
not propagated out of `converse`.)

## Resource & trust posture

Eval code is **medium-trusted (sandbox-style)**. So:

- **All config is Rust-fixed. Haskell sets nothing.** `converse` takes no config
  object — model, temperature, per-call max-tokens, `maxSteps`, total token/cost
  budget, compaction threshold are all server-side. Same posture as the existing
  200-call rate cap and 30s timeout. The eval author cannot raise a ceiling.
- **Runaway guard is a Rust ceiling.** If the model keeps calling tools past
  `maxSteps` (or the token budget), the loop stops with an error `Value`. Not a
  Haskell knob.

(Revisit later: a single server-fixed model means you can't pick a cheap model
for a one-shot classify vs a strong one for a hard loop. Acceptable for v1's
"minimal intelligence" scope; a candidate for the first config relaxation.)

## Fork & lifecycle

- `fork :: Conv -> M Conv` — new id, **copy-on-write** share of history-so-far.
  Mutating either side after the fork must not bleed across.
- **Memory-only, session-scoped.** No disk format to design. A `Conv` survives
  turn-to-turn within a session but dies on server restart. Document this in the
  verb docs so nobody leans on durability.
- GC'd at session teardown (or explicit close, if we add one).

## Compaction (deferred to v2 — seam only)

**v1 is append-only** (cache-optimal, keeps Rust dumb). The append-only history
covers the ephemeral/minimal case; if the prefix would exceed the Rust ceiling,
the loop stops with a budget error.

v2: **LLM-summarize** old turns into a synthetic message. Triggers *rarely* —
threshold around **~20 messages** — so the event is infrequent and the long-
context savings are worth its cost. Design the `send` handler so inserting a
"summarize before completing" step is additive.

### Caching tension (the reason compaction is deferred and rare)

Summarize compaction **rewrites the message prefix**, which invalidates the
prompt cache from that point and spends a hidden genai call. This isn't specific
to summarize — *any* prefix-mutating compaction is cache-hostile; only append is
cache-safe. The ~20-msg threshold makes the cache reset a rare, amortized event.

For the same reason, **final-answer detection avoids API-level structured
output** (which would mutate the request shape per call):

- Default: an assistant message with **no tool calls** ends the loop (raw
  text/Value).
- When structured output is wanted: inject a synthetic **`submit` tool** whose
  arg schema *is* the result schema. It's just another stable tool in the table
  -> fully cache-friendly. The model ends by calling `submit`. Describe the
  schema in the tool spec / user message text, **not** via forced
  `response_format` / `tool_choice`. (Final mechanism still soft — "depends on
  session length"; this is the cache-biased default.)

## Implementation notes (when we build)

- New effect mirrors `LlmHandler` (`tidepool-handlers/src/lib.rs:1403`): genai
  client, but using **function-calling** — `ChatRequest` with `tools`, parse
  `tool_use` responses into `Invoke`, thread `tool_result` back on `resume`.
  This is the real lift; conversation-as-object and (later) compaction are easy.
- Constructors + helpers declared in `effect_decls.rs` alongside `llm`/`ask`;
  reuse the `Schema` ADT + `schemaToValue` for `tArgs`.
- Resident state mirrors the KV pattern (`Arc<Mutex<HashMap<ConvId, …>>>`),
  cloned per session, but memory-only (no disk flush).
- `tRun` calling `ask` parks the worker via the existing `ReplAskDispatcher`
  path (`tidepool-repl/src/ask.rs`); the `Conv` is idle (already returned its
  `Invoke`, waiting on `resume`) and survives the park.

## Open / TODO

- [ ] **Turn shape** — confirm flat batch `Invoke` vs an exposed continuation
  form. Leaning flat + machinery-driven.
- [ ] **Final-answer mechanism** — implicit no-tool-call vs explicit `submit`
  tool; "depends on session length / caching." Cache-biased default = `submit`
  tool when a schema is needed, implicit otherwise.
- [ ] System-prompt *accumulation* (the original "accumulating system prompt") —
  a `steer`/`refineSystem` mid-loop nudge as a second channel. Not in v1.
- [ ] Streaming (`Response::Stream`) for token-level output. Not in v1.
- [ ] Single server-fixed model -> per-call model choice as first config
  relaxation.

## Decisions log

| Question            | Decision                                                        |
|---------------------|-----------------------------------------------------------------|
| Threading model     | Handle-mutable + `fork` (new id)                                |
| Tool execution      | Haskell owns all tools; Rust knows only schemas                 |
| Tool errors         | Feed back as error result; model retries                        |
| Surface (v1)        | High-level `converse` only; low level built but unexposed       |
| Config ownership    | All Rust-fixed; Haskell sets nothing                            |
| Runaway guard       | Rust ceiling (`maxSteps`/budget), sandbox posture               |
| Fork storage        | Copy-on-write                                                   |
| Lifecycle           | Session-scoped, memory-only, dies on restart                    |
| Compaction          | Deferred to v2; LLM-summarize at ~20 msgs (rare, amortized)     |
| Caching             | Append-only v1; avoid forced structured output (`submit` tool)  |
