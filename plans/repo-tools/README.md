# Repo-defined tools — design (v0)

A second Tidepool MCP server whose tools are **durable, typed Haskell modules** in
`.tidepool/mcp/`, iterated and tested like code — not improvised per call. It
reuses the eval compile-path (extract → CBOR → JIT); a tool is "an eval with a
fixed body, a name, and types."

This directory is the design, split for review. **LSP is deferred to v1** (see
[lsp.md](lsp.md)) — v0 is the typed-tool substrate + server + a couple of tools,
enough to answer the one question that matters.

## The bet (and the honest doubt)

An ad-hoc `eval` is throwaway: the agent re-derives the same clever program every
time, quality varies per write, nothing accumulates. The bet is that a repo should
host **tool definitions** — typed, tested, iterated programs the agent *calls*
instead of re-deriving — and that this beats improvising.

The honest doubt, kept here because it's the actual risk: **we are not sure this
clears escape velocity.** We have not yet found a framework-only example so
compelling you'd reach for the tool over writing the eval fresh. v0 exists partly
to find out. The thing that most plausibly clinches it — semantic *composition* via
LSP — is exactly what we're deferring, so v0 is a genuine test of whether the
substrate (typed durable tools + hot-reload self-extension) is worth anything on its
own. See [tools.md](tools.md) for the reckoning; don't let the machinery's elegance
(it's real — every mechanism is a passing spike) launder the open question of
usefulness.

## Why "write-once" changes everything

A tool is no longer a snippet, it's **code** — a data type, deriving, instances.
That's the unlock: durable modules keep their types at the boundary instead of
throwing them away for convenience. `run :: Args -> M Result` over real records and
sum types gives a typed contract (compiler catches drift), schemas derived from the
types — tool input, tool output, *and the LLM calls inside the tool* (one shared
`FromValue` parser, plus `McpSchema`/`LlmSchema` renderers split by boundary) — and
typed tests. An ad-hoc eval can have none of these.

It also yields **self-extension**: with hot reload, an agent writes a tool module →
reloads → calls it, growing its own toolbelt mid-session (see [server.md](server.md)).

## Scope

**v0 (this plan):**
- the typed-tool framework — [framework.md](framework.md)
- the MCP server + hot reload — [server.md](server.md)
- 2–3 framework-only tools + the usefulness verdict — [tools.md](tools.md)
- build / swarm execution — [execution.md](execution.md)

**Deferred to v1:** the LSP effect (the composition story, the eventual
escape-velocity bet) — [lsp.md](lsp.md); CBOR distribution / no-GHC consumers;
multi-language; cross-repo portability.

**Why LSP is deferred, not dropped:** it's the marquee capability, but (1) the
framework must exist before any tool can be built on it; (2) it's a large stateful
subsystem (crate + held session); (3) rust-analyzer's stability on *this* workspace
is itself unproven — the harness's native LSP crashes here ([lsp.md](lsp.md)
Checkpoint 0); (4) v0 tests the substrate without it. LSP rides on a proven
framework, not the reverse.

## Settled decisions (cross-cutting — details in each doc)

- Separate server binary (`tidepool-tools`); the eval server is untouched.
- Typed both ends; records + sum types; schema authored in-Haskell as a `Value`,
  checked against the type via `ToSchema`/`conformsTo` at boot.
- Hand-rolled generic parser (vendored aeson's generic `FromJSON` is stripped).
- Sum types → aeson `TaggedObject` (so `ToJSON` is free and the derived schema
  matches by construction).
- Static registration is the real surface; `--debug-fast-iteration` adds hot-reload
  meta-tools.

## Feasibility headline

Every generic mechanism the framework needs is a **passing JIT spike** (field
reflection, `Maybe`-optionality, `ToSchema` incl. nested `[Text]`, generic
`Value→record` parse). Details + table in [framework.md](framework.md). The design
is de-risked; the *usefulness* is not — that asymmetry is the thing to keep in view.

## Open questions (cross-cutting)

- **Is the substrate useful without LSP?** The central one. v0 must answer it
  honestly ([tools.md](tools.md)).
- **Does v0-without-LSP under-test the thesis** so badly the verdict is
  inconclusive either way — i.e. is a thin LSP slice actually needed in v0?
  (Tension with the deferral; flagged for Kimi.)
- **Is a swarm even warranted for v0's size?** Without LSP, v0 is small
  ([execution.md](execution.md)).
- **Where's the real recurring, judgment-dense task** a tool would win at? We have
  candidates, not a proven killer.
