# Wave 3 — `boot-onecompile`: item 0 steps 4–5

**Status: EXECUTED 2026-08-11** (lane `spawn-latency`, dev `wave3-fusion`,
commit `838862ba`). Both holds cleared before it ran: wave 2 folded and
`--targets` landed on both sides.

**Measured:** pre-model extract spawns **2 → 1**; pre-model boot path median
**22.449s → 11.664s** (−48.0%), non-overlapping distributions. Full receipt,
including an invalid first A/B that was caught and redone under matched
conditions, in
`plans/post-restart/extract-wave/spawn-latency/05-wave3-measurement.md`.
Item 0's end state (exactly ONE GHC compile pre-model) is reached.

**Two claims in this spec were STALE at execution time** — corrected in
`plans/post-restart/extract-wave/spawn-latency/04-turn-latency-plan.md` §2,
which is the spec the work was actually built against:

1. The whole "extract side" section below was already DONE (`--targets` landed
   as its own item; `compile::compile_turns` is the Rust entry). This reduced
   to driver-side fusion with no Haskell edit — as this file's own premise
   correction predicted it would.
2. "render additionally splices `__selfHarnessCompaction`" is **false at
   HEAD**: the compaction summary is composed in Rust after the render run, so
   render and loop splice byte-identical `state_in(prior_state)` text. One
   splice, not two — the fusion was sounder than argued here.

**One obstacle this spec did not mention:** the turn template hard-codes a
single entry binder (`result`), so a fused module needed a second one. Resolved
by an additive `TurnTemplate::extra_entries` rendered through the same code
path as `result` — NOT a hand-written entry in the `helpers` slot, which would
have re-created the hand-maintained-copy mechanism this wave's ledger keeps
catching.

---

<details>
<summary>Original spec as written (HELD status, stale sections retained for
history)</summary>

**Status: HELD** on TWO things now — wave 2's fold (it rewrites the same three
functions) and the `--targets` prerequisite (`03-targets-prereq.md`).

> **PREMISE CORRECTED 2026-08-09.** This spec was written believing multi-target
> emission already existed as "Phase B's multi-binder machinery". It does not
> — see `03-targets-prereq.md` for the verification. The "extract side" section
> below therefore describes work that now belongs to that prerequisite item,
> NOT to this one. When `--targets` lands, this spec reduces to the driver-side
> fusion only: build one module with both helper decls and both targets, call
> the multi-target compile once, and hand the pre-compiled loop to
> `run_loop_fragment_inner`. Re-read this file against the landed `--targets`
> shape before spawning; do not implement the extract side twice.

The base merge has landed; anchors below were re-derived post-merge, but
re-grep before trusting any of them.

## Goal

Emit `render` and `loop` from ONE extract invocation, so the outer session pays
ONE GHC compile before the first model call instead of two. The outer machine
boots from the render target; the loop lands as the second JIT function in the
same machine.

Combined with wave 2 (both seeds gone), this is the item's end state: exactly
one GHC compile pre-model.

## Why the fusion is sound (checked against the post-async `run_one_cycle`)

`run_one_cycle` (driver.rs 674) renders the pre-loop prompt and runs the loop
against **the same `prior_state`**:

```
let prompt_before = self.render_framing(prior_state, prior_compaction)?;   // 1551
…
let (value, table) = self.run_loop_fragment(prior_state).await?;           // 939 → 981
```

Both splice `state_cross::state_in(prior_state)` into their helpers; render
additionally splices `__selfHarnessCompaction`, which is also known before the
render call. So one module can carry both helper decls and two compile targets.

The POST-loop render (`render_framing(Some(&state_json), …)`) uses the NEW
state and cannot fuse — it stays a separate compile, and it is after the first
model call so it is outside the pre-model count. `render_framing` is `pub` and
driven directly by acceptance tests; **keep it**.

## The extract side — multi-target from one GHC session

`compile_turn` (`tidepool-harness/src/compile.rs` 104) is single-target: it
passes one `--target` and reads `{target}.cbor` + `meta.cbor`. On the Haskell
side `writeWholeModuleClosed` (`haskell/app/Main.hs` 333) translates the module
closed around ONE target and writes ONE `meta.cbor`.

Shape of the change:

1. Split `writeWholeModuleClosed` into (a) a per-target translate+collect step
   returning nodes + metadata pieces + runLLMTurn sites, and (b) a single
   write step that emits one `<target>.cbor` per target and ONE `meta.cbor`
   merged across all of them via `mergeMetaPreserving`. Keep
   `mergeMetaPreserving`'s loud collision behaviour — do not paper over a
   collision to make the merge succeed.
2. Accept repeated `--target` on the CLI. Preserve the existing single-target
   contract exactly (`--target foo` → `foo.cbor`) — every other caller depends
   on it, and `processSessionFile` shares this function.
3. `meta.cbor` is shared, so its scalar fields need a defined multi-target
   meaning: `has_io` is the OR across targets, `var_names` the union.
   Both are diagnostic (`tidepool-repr/src/serial/read.rs` 82–88), so a union
   is honest — but WRITE DOWN the rule where the merge happens.
4. Rust side: a multi-target `compile_turn` returning one `CompiledTurn` per
   target sharing the merged table. Keep the existing single-target entry point
   as a thin wrapper so nothing else has to change.

**Both tables are the same merged table**, which is the point: the session's
`merge_table` is monotone and the render target's table therefore already
carries the loop's constructors — including the RunLLMTurn ConTags the machine
needs. That removes the pure-render ConTags hazard wave 2 pins, rather than
merely relying on `add_function`'s re-resolution to heal it.

## The driver side

- Add a fused entry (e.g. `compile_cycle_entry`) that builds ONE module with
  both helper decls (`__selfHarnessState`, `__selfHarnessCompaction`) and both
  targets, and returns both compiled turns.
- `run_one_cycle` calls it once; passes the render turn to a render-run path
  and hands the loop turn to `run_loop_fragment` / `run_loop_fragment_inner`
  (981), which must now ACCEPT a pre-compiled loop instead of calling
  `compile_outer` itself.
- `render_framing` (1551) stays as-is for the post-loop render.
- `compile_outer` (631) stays for any remaining single-target use.

Do not change what is IN the row, the qualified-import scheme
(`state_cross::LOADED_QUALIFIER`), or the `outer_decls()` decl list. The
qualification exists to dodge an ambiguous-occurrence clash with
`Tidepool.Prelude`; a fused module has MORE names in scope, not fewer, so
re-check that specifically.

## Verification (the wave's full gate — step 4 touches extraction)

- hardened differential with floors intact:
  `TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh tidepool-codegen --run-ignored all -E 'test(haskell_suite_differential)'`.
  **`COMPARED_FLOOR` must not drop.**
- `corpus_report` — same shape, `-E 'test(corpus_report)'`
- extract-fidelity-test — EVERY check passes; report the actual N/N (the total MOVES as checks are added; it is context, never a target):
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- bash -c 'cd haskell && cabal test extract-fidelity-test'`
- harness acceptance: `scripts/battery-shard.sh tidepool-harness -E 'binary(/^acceptance_/)'`
- **the item's receipt**: `acceptance_boot_compile_count` — drop its constant
  to what you measure. With wave 2 folded, the target is 1.
- Extractor id-stability is PINNED (`session_table_qualified_identity` + two
  quick-tier assertions). A multi-target merge that changes a DataConId fires
  them. If it does, STOP and escalate — that is a design conversation, not a
  test to silence.

## Known fold point

`haskell/app/Main.hs` is shared with sub-TL `spawn-latency`: its D1 reworks
`writeWholeModuleClosed`'s metadata merge (~line 348) — the same function this
splits. Per the realm-spike conflict experiment these are deliberately NOT
pre-partitioned. Write a minimal localized diff, do not reorganize surrounding
code, and log anything non-mechanical in `LEDGER.md` at fold.

</details>
