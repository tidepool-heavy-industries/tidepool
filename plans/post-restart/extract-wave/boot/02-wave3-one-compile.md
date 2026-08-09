# Wave 3 — `boot-onecompile`: item 0 steps 4–5

**Status: HELD** on wave 2's fold (it rewrites the same functions) and on the
`root.harness-lifecycle` base merge. Anchors below are post-async; re-verify
after both.

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
- extract-fidelity-test 26/26:
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
