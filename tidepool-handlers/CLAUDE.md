# tidepool-handlers — concrete effect handlers (`src/lib.rs`, single file)

The Rust side of every `<Eff>Req` — Console, KV, Fs, SG, Http, Exec, Lsp, Llm,
plus the debug-only Meta handler. `build_base_stack`/`base_decls_with_ask`
assemble the fully-wired server. See root `CLAUDE.md` for the project map;
`tidepool-mcp/CLAUDE.md` for the Haskell-facing half of the effect contract
(`*_decl()` + the eval-authoring patterns) — this doc covers the Rust side of
that same contract in more depth.

## Adding a new effect constructor here

Each effect is (usually) a `// === Tag N: Name ===` section: a
`#[derive(FromCore)] enum <Eff>Req` (one variant per constructor, `#[core(name
= "...")]` mapping to the Haskell GADT constructor name 1:1), a handler
struct, `impl DescribeEffect` (returns the `tidepool_mcp::*_decl()`), and
`impl EffectHandler<CapturedOutput>` whose `handle` match has one arm per
variant. **The `Tag N` numbering is not dense or universal** — it follows the
base-stack `HList` position for Console..Llm, but the Lsp section (`Lsp:
semantic queries...`, sits between SG and Http, NOT tag-numbered) and the
debug-only Meta section break the pattern; don't assume you can count tags to
find a section. Adding an operation to an EXISTING effect = one new enum
variant + one new match arm + (usually) an added constructor in the matching
`tidepool-mcp` `*_decl()`. A wholly new effect type needs a new positional
union-tag slot (see root `CLAUDE.md`'s locked-decision on union tags).

## `cx.respond*` — pick by result shape, not habit

- **`respond(val)`** — the default. One value, converted eagerly via `ToCore`.
- **`respond_caught(result)`** — wraps a handler `Result` as `Right v` /
  `Left msg` instead of aborting the eval on failure; this is the substrate
  for `try*` verbs (failure isolation for long-running orchestrations, e.g. a
  bad HTTP call becomes a catchable `Left`, not a killed eval). Only
  `EffectError::Handler` becomes `Left` — structural/bridge/eval errors still
  propagate unchanged (the line between failure *isolation* and corruption
  *hiding*; see the `respond_caught_structural_err_propagates_not_swallowed`
  test).
- **`respond_stream(iter)`** — parks an arbitrary (possibly infinite) Rust
  iterator; the JIT consumes it lazily. Use for open-ended/unbounded sources.
- **`respond_list(vec)`** — an owned `Vec<T>` exposed lazily at ELEMENT
  granularity: list cells materialize eagerly, but each cell's head is a
  thunk that converts its element to a `Value` only when forced (memoized).
  `take 3` converts 3 elements; `length` converts none. Use for a known-size
  collection where callers commonly only need a prefix (search hits, LSP
  nodes) — `SgReq`/`LspReq::Where` use this.

## Sandboxing

Fs/Sg/Exec are rooted at `HandlerConfig.cwd` (the workspace/session sandbox).
Path resolution canonicalizes both the sandbox root and the target path, then
checks `starts_with` — any path resolving outside the root is a loud
`"path escape: ... is outside sandbox"` / `"Path escapes sandbox: ..."`
error, not a silent clamp. This is enforced per-call at the handler, not once
at startup — a symlink or `..` component escaping the sandbox is caught
after canonicalization, not before. **Lsp is NOT part of this
canonicalize+`starts_with` check** — it forwards node/file addressing to the
`tidepool-lsp-daemon` sidecar, which gates access through its own
`registry::server_for` workspace-root binding instead (see `tidepool-lsp`'s
`CLAUDE.md`).

## Lsp handler specifics

`LspHandler` (~line 960 of 3062 — between the SG and Http sections, roughly a
third of the way into the file, NOT near the end) is a thin Unix-socket
client to the `tidepool-lsp-daemon` sidecar (`tidepool-lsp` crate — see its
`CLAUDE.md`). No daemon running yields an immediately actionable error:
`"no LSP daemon at <path> — start tidepool-lsp-daemon in the workspace"`
rather than a hang or opaque connection error. `LspNode` (the `Node` wire
type) carries an exact `{name, container, kind, file, pos:{line,char}, text}`
— `pos` is the real UTF-16 position the daemon resolved, so re-addressing a
node doesn't re-search by substring.
