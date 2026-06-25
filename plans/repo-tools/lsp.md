# lsp.md — LSP effect (DEFERRED to v1)

**Status: not in v0.** Documented so v0's framework/server decisions stay
LSP-compatible, and because it's the eventual escape-velocity bet. Pull it forward
only on a deliberate call.

## Motivation — composition, not access

The agent already has *one-shot* LSP (Claude Code's native tool; other clients often
have none). What no client can do one-shot is **fold many semantic queries with
small-LLM glue**: walk a call hierarchy transitively, filter deterministically,
`ask`/`llm` only at the irreducible points — all as one coroutine whose intermediate
results live in the JIT *heap*, not the agent's context window. The product
`semantic queries × small-LLM glue × heap-resident state` is the non-duplicative
core.

Marquee demonstrator — **transitive call-graph walk:** `incomingCalls` is one level;
one-shot it dumps a level into context per call. A tool BFS-walks it in the heap,
accumulates the full reverse-call graph (which would blow the context window), and
returns a *verdict* (blast radius, or `ask` only where a caller's intent is
ambiguous). Neither a bare LSP call nor a raw agent can do this.

And the server serves **whatever client connects** — most (other models, CI agents)
have no native LSP, so for them these tools are also raw access they lack, plus a
**managed, restart-on-crash session** more robust than a per-client plugin. The value
doesn't hinge on any one client's built-ins.

## Why deferred (honest)

1. The framework must exist before any tool — LSP rides on it.
2. It's a large stateful subsystem (crate + held session + JSON-RPC + lifecycle).
3. **rust-analyzer stability on this workspace is unproven** — see Checkpoint 0.
4. v0 can test the substrate without it.

It's the *clincher*, sequenced after the foundation — not dropped.

## Checkpoint 0 (gates any LSP work)

**Prove rust-analyzer runs stably on this mixed Rust+Haskell workspace at all** before
building. The harness's own native LSP crashes here (rust-analyzer exceeds 3
crash-recovery attempts on every call — `findReferences`/`hover`/`incomingCalls` all
failed during design). Cause unknown — workspace size / config / toolchain. If
rust-analyzer is unstable here, a restart-on-crash manager and possibly a scoped
workspace / `rust-project.json` are load-bearing. The native LSP tool can't be a
validation oracle while it's down; the crate self-tests against a directly-spawned
server.

## Design (preserved for v1)

- **Crate facade** — `tidepool-lsp` (standalone, crate-tested) seals subprocess
  lifecycle, JSON-RPC framing/correlation, indexing-readiness, and multi-step
  interactions (rename = prepare+rename internally). "Haskell expands, Rust
  collapses" — the stateful collapse lives on the Rust side.
- **Server-scoped session** — an `LspManager` owns rust-analyzer for the server's
  life; tools issue stateless queries; no session handle crosses into Haskell; `Fs`
  writes notify the manager.
- **Synchronous request→result effects, no subscription** — simpler; diagnostics
  (push-based) deferred (addable later as synchronous pull `textDocument/diagnostic`).
- **Mutating ops return edit-sets as data** — `rename → [Edit]`, applied via the
  existing `applyEdits`/`planUpdate` verbs (preview-able, conflict-checkable). Composes
  with the editing surface already shipped.
- **Proposed effect surface** (mirrors a clean crate API; includes the call-hierarchy
  ops the composition thesis makes central):

```haskell
data Pos = Pos { line :: Int, col :: Int }
data Location ; data Edit ; data Symbol ; data CallSite

lspFindUsages       :: FilePath -> Pos -> M [Location]
lspGotoDefinition   :: FilePath -> Pos -> M (Maybe Location)
lspImplementations  :: FilePath -> Pos -> M [Location]
lspWorkspaceSymbols :: Text -> M [Symbol]
lspTypeAt           :: FilePath -> Pos -> M (Maybe Text)
lspRename           :: FilePath -> Pos -> Text -> M [Edit]      -- edits as DATA
lspIncomingCalls    :: FilePath -> Pos -> M [CallSite]         -- marquee
lspOutgoingCalls    :: FilePath -> Pos -> M [CallSite]
```

## Open questions

- **Call-hierarchy in the contract** — proposed yes (it's the marquee); confirm.
- **Does composition genuinely beat N native calls** enough to justify the subsystem,
  for Claude Code specifically? (For non-LSP clients the case is clearer.)
- **Diagnostics** — synchronous pull later; worth it?
- **Bridge** — `tidepool-bridge` derives for the LSP types (plain records/tuples)?
- **Multi-language** — config per language (HLS, pyright) once rust-analyzer proves.
