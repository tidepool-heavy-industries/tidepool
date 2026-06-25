# tools.md — the v0 tools + the usefulness reckoning

## The bar (and why most "tools" don't clear it)

A tool earns its place only if it encodes **accumulated, irreducible judgment that's
annoying to re-derive and that recurs** — compute the 95% deterministically, `ask`
the 5%, and *remember the answer so it never re-asks*. A wrapper around things the
agent already does cheaply is just the agent, indirected. Mechanism-demos
(propose-pick, unwrap→rewrite) prove the plumbing, not the value.

**This is the open question of v0, stated plainly:** without LSP (the composition
capability, deferred — [lsp.md](lsp.md)), do *framework-only* tools clear the bar, or
is semantic composition the thing that actually has juice? We don't know. v0 must
answer it without grading on a curve. The machinery being elegant (it is — see
[framework.md](framework.md)) is not evidence the tools are useful.

## The ration-attention pattern

The combinators, rebuilt on the typed `ask`, are how a tool rations attention:
```haskell
escalate :: (a -> Maybe b) -> (a -> M b) -> a -> M b      -- cheap path, else ask
bisectM  :: (a -> M Bool) -> [a] -> M (Maybe a)
triageBy :: (a -> k) -> (k -> [a] -> M v) -> [a] -> M [(k,v)]
```
A tool computes the bulk, escalates only the irreducible cases, and (with KV /
fixtures) accumulates verdicts so the same case never re-asks. Because tools inherit
eval's suspension ([server.md](server.md)), an `ask` *is* a coroutine yield.

The glue is *typed* too: the escalate path uses `askAuto`/`llmAuto`
([framework.md](framework.md)), so an irreducible case returns a real `Decision` sum
to dispatch on — not a `Value` to optic — and a misclassification can't smuggle in a
fourth option. Same shared `FromValue` parser as the tool boundary; the LLM schema
comes from `LlmSchema` (tool I/O uses `McpSchema`).

## v0 candidate tools

- **`divergence-triage`** — the strongest framework-only candidate. The corpus
  harness already replays real -O2 Core through JIT-vs-eval and 5-way classifies; the
  *irreducible* part is "is this divergence a real bug or a known-acceptable one" —
  judgment that currently lives in scattered notes and is re-derived every regression
  hunt. The tool auto-classifies the clear cases, `ask`s only on a novel divergence,
  and accumulates verdicts so the same one never re-asks. Recurs in the actual dev
  loop; re-deriving it as a fresh eval each time genuinely hurts.
- **`gotcha-guard`** — the GHC-Core gotcha catalog (the numbered list in CLAUDE.md:
  `tagToEnum#`, joinrec, eqSpec arity, strict-let error bindings, …) encoded
  *executably* against a diff/fixture, instead of prose a human re-reads. Ask on
  near-misses. Accumulated judgment = the catalog itself.

## Verdict criteria (pre-registered)

Fixed before dogfooding, to avoid grading on a curve: **"≥2 tools I'd reach for again
next session over writing the eval fresh."** If framework-only tools don't clear it,
that's a real signal — maybe LSP composition is required for escape velocity, or
maybe the modality needs rethinking. Either is worth knowing.

## Open questions

- **Are these the right examples?** Or is the killer recurring task somewhere we're
  not seeing in the actual workflow?
- **Does ration-attention need richer `ask`/`llm`** (structured asks, small-LLM glue)
  than we have to be ergonomic?
- **How to measure "useful"** beyond the gut-check verdict — track reach-for rate
  across sessions?
- **Does v0-without-LSP under-test the thesis** so much the verdict is inconclusive
  either way? If so, is a thin LSP slice actually needed in v0 to get a real signal?
  (Direct tension with the deferral — Kimi's view wanted.)
