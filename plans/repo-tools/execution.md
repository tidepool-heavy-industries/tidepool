# execution.md — building v0

Without LSP, v0 is **small** — and that's the first honest question: is a swarm even
warranted, or is this 1–2 focused streams?

## Streams

- **S3 — framework** ([framework.md](framework.md)). Pure Haskell lib; port the four
  proven spikes, extend to sums + output-direction; eval-tested. No deps.
- **S4 — server harness** ([server.md](server.md)). The `tidepool-tools` binary;
  load/compile/register, hot-reload meta-tools, dispatch. Depends on S3's `ToolDef`/
  `Server` contract (C3) — codeable against the frozen contract.
- **S5 — tools** ([tools.md](tools.md)). Framework-only (`gotcha-guard`,
  `divergence-triage`) after S3+S4.

(Numbering keeps continuity with the full plan, where S1/S2 were the deferred LSP
crate + effect — see [lsp.md](lsp.md).)

## Frozen interfaces

Freeze before any parallel work:
- **C3** — `ToolDef`/`Server`, the typeclasses (`HasSchema`/`FromValue`),
  `mkTool`/`mkToolAuto`/`conformsTo` signatures.
- **C4** — the server load/dispatch contract (how Rust compiles the aggregate,
  extracts `[ToolDef]`, registers, dispatches; the meta-tool surface).

## Waves

- **Wave 0 — freeze + scaffold** on the shared base before any worktree forks: the
  C3/C4 contracts, plus the physical skeleton (empty `tidepool-tools` bin + Cargo
  membership, `.tidepool/mcp/{Server.hs, lib/Tool.hs}` placeholders). Under worktree
  isolation, the workspace manifests are the only shared files — pre-wiring them keeps
  parallel worktrees from colliding.
- **Wave 1 — S3 ∥ S4**, independent worktrees, each tested against its contract
  (eval-driven framework tests for S3; a hardcoded sample `Server.hs` for S4).
- **Wave 2 — S5 tools**, after S3+S4 merge.

## Worktree discipline

Post-Wave-0 the two streams touch disjoint files (S3: `.tidepool/mcp/lib/`; S4:
`tidepool-tools` + the server crate), so merges stay clean. The only shared files are
the Wave-0-frozen manifests.

## When LSP lands (v1)

LSP adds one stream (the `tidepool-lsp` crate + `Lsp` effect, gated on Checkpoint 0)
on top of the proven framework. v0's frozen contracts are designed to accommodate it
without rework.

## Open questions

- **Is parallelism worth it for v0's size?** Possibly just sequential — S3 → S4 → S5,
  one stream — since swarm overhead may exceed the gain at this scale. Decide once the
  contracts are frozen and the size is concrete.
- **Who owns the shared compile-path extraction** (S4 needs it as a library)? May
  warrant a Wave-0 refactor of the eval server to expose it.
