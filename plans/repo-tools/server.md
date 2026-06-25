# server.md — the MCP harness + hot reload

## Motivation

Load the durable tools, expose them as ordinary MCP tools, and make iteration fast
— edit a module, see the change, without `/mcp` reconnects. A **separate binary** so
the load-bearing eval server is untouched.

## Architecture

A new binary (`tidepool-tools`) that, at boot:
1. discovers `.tidepool/mcp/Server.hs` (the aggregate);
2. compiles it **once** via the eval compile-path (extract → CBOR → JIT) — the whole
   tool surface in a single pass, one cache entry;
3. evaluates `server :: Server`, extracts `[ToolDef]`;
4. asserts `conformsTo` per tool, **fails loud** on drift;
5. registers each in `tools/list`.

`tools/call name args` evaluates that tool's `tRun` against the call `input`,
reusing the cached compilation — per-call cost is a JIT run, not a recompile.

**One aggregate, not N per-tool compiles:** the whole MCP surface is a single typed
value (`Server`), compiled once. `tools/list` is one eval; every call reuses it. For
v0 the aggregate is hand-listed (write the module, add a line to `server`); codegen
that scans `tools/*.hs` is deferred.

## Dispatch + hot reload

- **Static registration is the real surface.** Tools appear first-class in
  `tools/list` at boot; production runs exactly these.
- **`--debug-fast-iteration` adds meta-tools** — `reload` (recompile the aggregate),
  `get-tool-schema name`, `call-tool name args`. These reflect a reload *instantly*,
  so an author or agent iterates without a reconnect. MCP can't reliably live-update
  a registered tool's schema, hence the generic call path for iteration; the static
  registration refreshes on reconnect. Ship prime-time *without* the flag → only the
  static tools, no generic surface.

## Self-extension

With `--debug-fast-iteration`, an agent can write a tool module → `reload` →
`call-tool` it, mid-session — growing its own toolbelt. A v0 capability that falls
out of hot reload, not a separate effort.

## Failure + suspension semantics (inherited from eval)

- A tool that throws → `CallToolResult::error` (not a dead connection).
- A tool that hits `ask` → **suspends** with a `continuation_id`, exactly like an
  eval; `resume` re-enters it. So a *tool can be a coroutine* — which is the whole
  point of ration-attention tools ([tools.md](tools.md)). Worth an explicit test.

## Open questions

- **Reserved names** — tool names vs `reload`/`call-tool`/`get-tool-schema`: reserve,
  error loudly at boot on collision.
- **Reload isolation** — a single tool's compile failure must not kill the server or
  the other tools; per-tool error surfacing on `reload`.
- **Shared compile-path** — the eval compile-path becomes a library both binaries
  link; factor it out cleanly (it currently lives in the eval server).
- **`tools/list_changed`** — emit for capable clients to refresh live? Client support
  is uneven; the meta-tools cover iteration meanwhile. Deferred.
- **Source layout** — `.tidepool/mcp/{Server.hs, tools/*.hs, lib/Tool.hs}` vs flatter.
  Lean: as written.
- **Trust** — tools run arbitrary Haskell, same as eval; no new exposure, but the
  self-extension loop means an agent's *written* code runs. Note it.
