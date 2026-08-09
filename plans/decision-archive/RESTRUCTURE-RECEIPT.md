# CLAUDE.md restructure receipt — 2026-08-08

Branch: `root.claude-md-restructure`. Scope per the spec: root `CLAUDE.md`,
all per-crate `CLAUDE.md`s, a new archive under `plans/`, and
`plans/README.md` links. No source code touched, no other `plans/` content
edited, no `memory/`/`.exo/` files touched.

## Method

Read all 9 `CLAUDE.md` files in full (root + `haskell/` + 7 per-crate),
built an operative-fact checklist (below), then grepped all 9 files for
historical/narrative markers (`2026-`, `historical`, `previously`,
`used to`, `no longer`, `legacy`, `deprecated`, `originally`, `superseded`,
`old blanket`, `already solved`, `back then`, `as of`, `measured
comparison`) to find candidates for the archive layer, cross-checked each
hit by hand.

**Finding, stated plainly:** most of these files already read as working
guides — dense, but current and load-bearing, not historical dogma. Root
`CLAUDE.md` in particular was already cleanly layered (a working guide
section + a separately-headed, verbatim Key Decisions Reference) with zero
embedded history to extract. The repo's own recent commit history shows
prior doc-hygiene passes (e.g. `78317e76 docs(runtime): drop references to
flags that no longer exist`), which likely explains why less genuine
historical cruft was found than the review's framing implied. Given the
hard constraint against inventing content-loss risk by moving things that
aren't actually historical, this restructure is conservative: two concrete
extractions (both in `haskell/CLAUDE.md`), one suspected-stale flag (in
`tidepool-codegen/CLAUDE.md`), and pointer infrastructure (archive dir,
index, root pointer, `plans/README.md` link) so future genuine history has
a clear home instead of accreting inline again.

## Three-layer structure, as landed

1. **Working guide** (per file) — build/test commands, env vars, current
   hazards. Unchanged in every file except the two extraction sites in
   `haskell/CLAUDE.md`.
2. **Architecture contracts** — root `CLAUDE.md`'s `## Key Decisions
   Reference`, byte-for-byte unchanged, still under `### Locked Decisions`
   authority framing (unchanged, verbatim): *"The Key Decisions Reference
   section below is the source of truth for all architectural decisions.
   Every entry is final. Do not deviate from locked decisions. Do not
   re-derive them. If you need a decision that isn't there, escalate to the
   human."* Per-crate files continue pointing at this section for
   architecture truth (each already opened with "See the repo-root
   `CLAUDE.md` for ... locked decisions" — pre-existing, left as-is).
3. **Archive** — new `plans/decision-archive/`: `README.md` (index) +
   `haskell.md` (the two extracted notes). Linked from `plans/README.md`
   under a new `## Reference` section, and from root `CLAUDE.md`'s Plans
   section (new `### Decision Archive` subsection explaining the split).

## Old → new mapping (checklist)

Every operative item enumerated in the task's hard constraints, mapped to
where it lives now. Everything not explicitly called out as "MOVED" stayed
exactly where it was (file unchanged for that item).

| Item | Old location | New location | Status |
|---|---|---|---|
| `TIDEPOOL_EXTRACT` (resolution, build commands, error text) | root `CLAUDE.md` Build & Test; `haskell/CLAUDE.md` "How the extract binary is resolved" | same | unchanged |
| `XDG_CACHE_HOME` (cache path root) | `tidepool-mcp/CLAUDE.md` "On-disk paths & config" | same | unchanged |
| `TIDEPOOL_EXPENSIVE_TESTS` | root `CLAUDE.md` Test tiers, tier 4 | same | unchanged |
| `ghc-slots.sh` (3 shared slots, auto-acquired by battery scripts) | root `CLAUDE.md` Test tiers preamble | same | unchanged |
| Test tiers 1–4 + "never run bare `scripts/battery.sh`" | root `CLAUDE.md` `### Test tiers` | same | unchanged |
| ~380s environment kill warning | root `CLAUDE.md` `### Test tiers` | same | unchanged |
| Locked Decisions authority framing (verbatim) | root `CLAUDE.md` `### Locked Decisions` | same | unchanged, verbatim |
| Key Decisions Reference (9 bullets: CoreFrame variants, no type variants, RecursiveTree, CBOR, Cast/Tick/Type erasure, HeapObject layout, GC, freer-simple continuations, union tags) | root `CLAUDE.md` `## Key Decisions Reference` | same | unchanged, verbatim |
| `cargo-nextest` rationale (process-per-test de-races JIT global state) | root `CLAUDE.md` Build & Test | same | unchanged |
| Eval Records API table (`Proc`/`Hit`/`FileRead`/`Commit`/`StatusEntry`/`FileDelta`) | root `CLAUDE.md` `## Eval Records API` | same | unchanged |
| Package layout / `cabal build` scoping (5 non-production test-suite stanzas) | `haskell/CLAUDE.md` "Rebuilding the Haskell Toolchain" | same | unchanged |
| Extract binary resolution / nix-profile wrapper / deploy dance | `haskell/CLAUDE.md` | same | unchanged |
| `dist-newstyle` per-worktree hazard | `haskell/CLAUDE.md` | same | unchanged |
| `scripts/redeploy.sh` | root + `haskell/CLAUDE.md` | same | unchanged |
| Fixture regeneration commands + `*_u<n>.cbor` pruning hazard | `haskell/CLAUDE.md` "Regenerating Test Fixtures" | same | unchanged |
| Haskell-extract diagnostic knobs (`TIDEPOOL_DUMP_CLOSED`, `TIDEPOOL_VARID_AUDIT`, `TIDEPOOL_JOINREC_DEBUG`, `TIDEPOOL_IFACE_DEBUG`) | `haskell/CLAUDE.md` Diagnostics table | same | unchanged |
| Eval stdlib module map + Structured LLM/Ask (`Schema`, `ask`/`llm`) | `haskell/CLAUDE.md` | same | unchanged |
| FormQQ module note — **self-referential doc-history clause** | `haskell/CLAUDE.md` (trailing clause) | `plans/decision-archive/haskell.md` §2; one-line pointer left in place | **MOVED** |
| Adding new Prelude functions (JIT dictionary polymorphism, `jit_surface.rs`) | `haskell/CLAUDE.md` | same | unchanged |
| `Map.Strict` knot-tying gotcha | `haskell/CLAUDE.md` Known Limits | same | unchanged (real, undated hazard) |
| Call-graph workspace-scoping rule (`transitiveLocal*` default) | `haskell/CLAUDE.md` Known Limits | same | unchanged (operative rule kept) |
| Call-graph scoping — **"already solved once, dated 2026-07-01" backstory** | `haskell/CLAUDE.md` (parenthetical) | `plans/decision-archive/haskell.md` §1; one-line pointer left in place | **MOVED** |
| JIT nested-child-run GC-rooting invariant ("segment 40") | `tidepool-codegen/CLAUDE.md` | same | unchanged — this is a current, load-bearing memory-safety invariant, not history; the "(segment 40)" label is cosmetic and was left as-is |
| JIT/effect-machine/cache diagnostic knobs (`RUST_LOG=tidepool::*`, legacy `TIDEPOOL_TRACE*`/`TIDEPOOL_FP_DEBUG` aliases, `TIDEPOOL_LAZY_RESULTS`, `TIDEPOOL_HEAP_VERIFY`, `TIDEPOOL_GC_POISON`) | `tidepool-codegen/CLAUDE.md` | same | unchanged |
| Shape/tag-mismatch trap machinery; `SeqOp`/boxed-array differential gaps | `tidepool-codegen/CLAUDE.md` | same | unchanged; see suspected-stale below |
| `RecursiveTree` flat-vector scheme, Enter/Exit walk pattern | `tidepool-repr/CLAUDE.md` | same | unchanged |
| `DataConTable.insert_checked` hygiene, sibling-group disambiguation | `tidepool-repr/CLAUDE.md` | same | unchanged |
| Session-id newtypes, `SessionModule` format, cross-language hash-minting invariant | `tidepool-repr/CLAUDE.md` | same | unchanged |
| CBOR wire format (`TPLR` header, version 2.0, metadata shape, golden test) | `tidepool-repr/CLAUDE.md` | same | unchanged |
| On-disk paths & config resolution (cache/config/project-local/CWD layering) | `tidepool-mcp/CLAUDE.md` | same | unchanged |
| Eval-authoring patterns (Aperture, Census, `update`/Edit/Diff/ast-grep tiers) | `tidepool-mcp/CLAUDE.md` | same | unchanged |
| `grepGlob` structural search verb | `tidepool-mcp/CLAUDE.md` | same | unchanged |
| MCP server internals (signal handling, preamble imports, eval timeout 600s/1800s) | `tidepool-mcp/CLAUDE.md` | same | unchanged |
| Effect-definition-macro contract (`*_effect_def!`, two projections) | `tidepool-handlers/CLAUDE.md` | same | unchanged |
| `cx.respond*` variant selection guide | `tidepool-handlers/CLAUDE.md` | same | unchanged |
| Sandbox enforcement (Fs/Exec/Lsp canonicalize+`starts_with`) | `tidepool-handlers/CLAUDE.md` | same | unchanged |
| 3 MCP tools + 1 resource (`session_run`/`session_resume`/`session_reset`/bindings resource) | `tidepool-repl/CLAUDE.md` | same | unchanged |
| Item classification (decl/stmt/meta/Auto), redefinition-replace semantics | `tidepool-repl/CLAUDE.md` | same | unchanged |
| Response shape (slim vs. `verbose:true`) | `tidepool-repl/CLAUDE.md` | same | unchanged |
| Launcher / env knobs (`TIDEPOOL_PRELUDE_DIR`, `TIDEPOOL_LLM_MODEL`) | `tidepool-repl/CLAUDE.md` | same | unchanged |
| Suspension (`ask`) semantics, session lifecycle internals | `tidepool-repl/CLAUDE.md` | same | unchanged |
| Daemon architecture, socket resolution (`--socket`/`$TIDEPOOL_LSP_SOCK`), 600s indexing-gate fallback | `tidepool-lsp/CLAUDE.md` | same | unchanged |
| LSP op surface boundaries (no trait-dispatch op, `diagnostics` fallback) | `tidepool-lsp/CLAUDE.md` | same | unchanged |
| Trampoline join-point evaluation, `Value` WHNF-only, `Heap` trait, `shapes.rs` ownership | `tidepool-eval/CLAUDE.md` | same | unchanged |
| Differential-testing harness list (how `tidepool-eval` is actually exercised) | `tidepool-eval/CLAUDE.md` | same | unchanged |
| Active plans index (one-spawn turn protocol, GHCi affordances, post-restart, PRDs 14/15/18) | `plans/README.md` | same | unchanged |

## What moved where, and what I did not touch

**Moved (2 items, both from `haskell/CLAUDE.md`, both pure doc-history
asides with no operative content):**
1. The call-graph-scoping "already solved once, dated 2026-07-01"
   parenthetical → `plans/decision-archive/haskell.md` §"Call-graph
   workspace scoping — why the note exists". The operative rule (default to
   `transitiveLocal*`) stayed in `haskell/CLAUDE.md`, word-for-word.
2. The FormQQ module note's self-referential closing clause ("unlike the
   old blanket list this passage used to describe") →
   `plans/decision-archive/haskell.md` §"`formqq-parser-test` module-listing
   note — prior wording". The current fact it was contrasting against
   (only `formqq-parser-test` lists `lib` as a build dependency) stayed in
   `haskell/CLAUDE.md`, word-for-word.

**Added (pointer infrastructure only, no content relocation):**
- `plans/decision-archive/README.md` (new index)
- `plans/decision-archive/RESTRUCTURE-RECEIPT.md` (this file)
- Root `CLAUDE.md`: new `### Decision Archive` subsection under `## Rules`
  (4 sentences) explaining the split and stating explicitly that the Key
  Decisions Reference is NOT archived.
- `plans/README.md`: new `## Reference` section (1 bullet) linking the
  archive.

**Not touched at all:**
- `tidepool-codegen/CLAUDE.md`, `tidepool-repr/CLAUDE.md`,
  `tidepool-mcp/CLAUDE.md`, `tidepool-handlers/CLAUDE.md`,
  `tidepool-repl/CLAUDE.md`, `tidepool-lsp/CLAUDE.md`,
  `tidepool-eval/CLAUDE.md` — grepped for the same historical-marker set
  described under Method, no hits that weren't either (a) current operative
  fact with a version/date tag still true today, or (b) a design-rationale
  sentence that is itself still load-bearing (explains a current
  architectural choice, not a past one). Nothing extracted.
- No source code.
- No `plans/` content beyond the new `decision-archive/` directory and the
  one-bullet addition to `plans/README.md`.
- No `memory/` or `.exo/` files.

## Suspected-stale (flagged for human review, NOT judged or moved)

- `tidepool-codegen/CLAUDE.md`, `SeqOp`/boxed-array differential-gap note:
  *"The proptest generator (`tidepool-testing`) does not currently emit
  `SeqOp` (checked 2026-07-07)... Also unexercised by the proptest generator
  today, so likewise latent rather than firing."* This is a dated
  "as-of" claim about test-generator coverage, over a month old at the time
  of this restructure (2026-08-08). It reads as still-plausible rather than
  clearly wrong, and it's a genuine operative fact (whether `SeqOp` /
  boxed-array primops are exercised affects whether their JIT-vs-eval gap is
  a live risk) — moving it to the archive on my own judgment risked losing
  operative content, so it was left in place. A human should re-verify
  against the current proptest generator and either refresh the date or
  drop the caveat if coverage has changed.
- No other dated/versioned claims were found across the remaining 6
  per-crate files (ticket references like `#317`/`#320`/`#335` are stable
  identifiers, not staleness risks).

## Line counts (before → after)

| File | Before | After | Δ |
|---|---:|---:|---:|
| `CLAUDE.md` | 210 | 219 | +9 (new Decision Archive pointer) |
| `haskell/CLAUDE.md` | 220 | 218 | −2 (net; 2 extractions, 2 one-line pointers added back) |
| `tidepool-codegen/CLAUDE.md` | 108 | 108 | 0 |
| `tidepool-repr/CLAUDE.md` | 92 | 92 | 0 |
| `tidepool-mcp/CLAUDE.md` | 156 | 156 | 0 |
| `tidepool-handlers/CLAUDE.md` | 103 | 103 | 0 |
| `tidepool-repl/CLAUDE.md` | 202 | 202 | 0 |
| `tidepool-lsp/CLAUDE.md` | 70 | 70 | 0 |
| `tidepool-eval/CLAUDE.md` | 61 | 61 | 0 |
| `plans/README.md` | 28 | 37 | +9 (new Reference section) |
| **New:** `plans/decision-archive/README.md` | — | 31 | new |
| **New:** `plans/decision-archive/haskell.md` | — | 46 | new |
| **New:** `plans/decision-archive/RESTRUCTURE-RECEIPT.md` | — | (this file) | new |

## Verify

Ran the required grep spot-checks post-edit — `TIDEPOOL_EXTRACT`,
`ghc-slots`, all 4 battery tier labels, and both `Locked Decisions`/`Key
Decisions Reference` headings all still resolve inside root `CLAUDE.md`
unchanged. Both new pointer sites in `haskell/CLAUDE.md` resolve to
`plans/decision-archive/haskell.md`, and both `plans/README.md` and root
`CLAUDE.md` link `plans/decision-archive/README.md`.

## Human-review flag

This restructure is submitted for human review per the task's fold
requirement. In particular: confirm the "conservative — extract only clearly
historical content, leave operative content in place even if dated" judgment
call was the right one, versus a more aggressive restructure the review may
have had in mind; and resolve the one suspected-stale item above.
