# One spawn per BLOCK: the Rust-side receipt

**Lane:** `batch-turns` → `batch-rust` child. Built against §8 of
`plans/post-restart/batch-turns-feasibility.md` (frozen wire contract), in
parallel with the `batch-extract` sibling implementing the Haskell side. No
Haskell file touched; no fixture regeneration; no live model calls.

---

## What landed

1. **`tidepool-extract-cmd`**: `ExtractCmd::turn_batch(path)` (`--turn-batch
   <plan.json>`) and `ExtractCmd::batch_out(dir)` (`--batch-out <dir>`),
   following `turn()`/`turn_out()`/`classify_out()`'s exact builder
   conventions (`tidepool-extract-cmd/src/lib.rs`). `ExtractCmd` stays the
   ONE invocation builder — the crate's own `no_workspace_crate_open_codes_an_extract_spawn`
   test still passes unmodified.
2. **`tidepool-runtime/src/diag.rs`**: `BatchDiagReport` /
   `BatchItemStatus` / `parse_batch_diag_report`, decoding §8's stdout
   document (`{version, diagnostics, items:[{index,status,dir,diagnostics}]}`).
   Implemented independently of `parse_diag_report` (not refactored to
   share code) specifically so this addition could not perturb that
   function's already-tested behavior — `parse_diag_report` is byte-for-byte
   unchanged.
3. **`tidepool-runtime/src/session/turn.rs`**: `run_turn_batch`, a sibling
   of `run_turn` — ONE new spawn site, performs exactly one
   `tidepool-extract --turn-batch` call.

## The shared-decode factoring

`run_turn`'s tail (the block that read `turn.cbor` off the output dir,
decoded it, and — for `Bind`/`Expr` — read `result.cbor`/`meta.cbor` off the
same dir) is now `decode_turn_output_dir(dir: &Path) -> Result<TurnResult, CompileError>`
(`turn.rs`, right after `run_turn`). **This is the one function both
`run_turn` and `run_turn_batch` read through.** `run_turn` calls it on its
own `temp.path()`; `run_turn_batch` calls it once per `"ok"` item on
`<batch-out>/i<k>/`. Neither `decode_turn_out` (the CBOR tag-dispatch) nor
`read_compiled_turn` (the `result.cbor`/`meta.cbor` reader) was duplicated —
`decode_turn_output_dir` is the only caller of either from the batch path,
same as it was `run_turn`'s only caller before this change. A batched item
and a per-item item cannot diverge in what Rust reads, by construction.

## `run_turn_batch`'s shape

- **Preflight** (before any spawn): every item's verdict must resolve to a
  supplied template via `TemplateSelector::for_verdict` +`select_template`,
  exactly as `run_turn` checks — a missing template is a caller wiring bug,
  reported as `CompileError::ExtractFailed` before any process runs.
- **`plan.json`** is built from `BatchTurnItem`s (turn_text, verdict,
  session_root, inject_modules, gen) into §8's wire shape, one spawn.
  Templates and `--include` are batch-wide (see Ambiguity 2 below).
- **Report handling**: `run_turn_batch` NEVER branches on the process's own
  exit code (see Ambiguity 1). It always attempts
  `parse_batch_diag_report` on stdout first; only an unparseable/malformed
  document, an over-long `items` array, an out-of-order `index`, an
  unrecognized `status`, or fewer item reports than requested with none
  marked `"failed"` become a top-level `Err(CompileError)` — i.e.
  "not attributable to a specific item," per §3's fallback invariant. An
  `"ok"` item decodes via `decode_turn_output_dir`; a `"failed"` item stops
  the loop and becomes `TurnBatchResult::failure` with
  `CompileError::Diagnostics` built from that item's own diagnostics
  (falling back to the flat top-level array if the item's own list is
  empty — both are populated in §8's own wire example, so this is
  belt-and-suspenders, not a contract deviation).
- **Types**: `BatchTurnItem`, `TurnBatchRequest`, `BatchItemFailure`,
  `TurnBatchResult` (new, `turn.rs`), re-exported from
  `tidepool-runtime/src/session/mod.rs` alongside `run_turn_batch` itself.

## Ambiguities in §8 encountered while implementing

These didn't block the build (each was resolved with the most conservative
reading), but the sibling and the parent should know about them:

1. **§8 does not pin whether the extract process's own exit code reflects a
   mid-batch compile failure.** A single-item `--turn` spawn exits non-zero
   on the user's Haskell failing to compile; it's unstated whether
   `--turn-batch` follows that convention (exit non-zero whenever ANY item
   fails) or instead always exits 0 when the process itself did its job
   (produced a valid report), leaving failure entirely in `items[].status`.
   **Resolved defensively**: `run_turn_batch` never reads `run.verdict`/exit
   status at all — it trusts the parsed report's own `items[].status`
   unconditionally, and only falls back to "not attributable" on a report
   that doesn't parse or doesn't make sense (see above). This works under
   either convention the Haskell side picks; it does NOT need to match
   whichever one lands.
2. **Are `templates`/`--include` per-item or batch-wide?** §8's item schema
   (`index, turn_text, verdict, template, session_root, inject_vals,
   bind_gen`) has no template-source or include-dir field, and the
   invocation signature shows `[--include <dir>]…` at the top level, outside
   `plan.json`. **Resolved as batch-wide**: `TurnBatchRequest::templates`
   and `::include` are supplied once, forwarded via the same
   `--turn-template`/`--include` repeated-flag mechanism `run_turn` already
   uses. `plan.json`'s per-item `"template"` field is redundant with
   `verdict` (it's `TemplateSelector::for_verdict(kind, binders).wire_name()`,
   spelled out on the wire rather than re-derived extract-side) — Rust
   computes and sends it, but never uses it as anything but that derivation
   forwarded.
3. **`TurnRequest::target`** (forwarded as `--target`, used by
   `tidepool-harness`'s shared-builder wrapper path) **has no field in §8's
   plan.json item shape.** `BatchTurnItem` therefore has no `target` field
   at all — a batch cannot currently carry a custom target binder per item.
   This is fine for every batchable shape in §5 (none of them need a
   non-default target), but if a future caller needs batched turns with a
   custom target, §8 needs an amendment, not a Rust-side workaround.
4. **The per-item `TurnOut` sidecar's filename inside `<batch-out>/i<k>/`
   is not spelled out** — §8 says the directory contains "exactly today's
   single-turn output set, byte for byte" without naming the sidecar file.
   **Resolved as `turn.cbor`**, matching `run_turn`'s own convention
   (`turn_out_path = temp.path().join("turn.cbor")`, unchanged by this
   lane) — `decode_turn_output_dir` reads `dir.join("turn.cbor")`
   unconditionally. The `batch-extract` sibling must write exactly that
   filename per item, or every `"ok"` item will fail to decode with a
   `CompileError::MissingOutput` naming the expected path (a debuggable
   failure, not a silent one, but named here to save the round-trip).
5. **Whether a `"failed"` item's own diagnostics field is guaranteed
   populated**, vs. only the flat top-level `diagnostics` array carrying
   real content. §8's wire example shows both populated identically.
   Implemented with a fallback (prefer the item's own `diagnostics`, fall
   back to the flat array if empty) so either sibling behavior works.

## Test list (stub-extractor, §8-shaped; all in `tidepool-runtime`)

All four required tests exist in `tidepool-runtime/src/session/turn.rs`'s
`mod tests` (`run_turn_batch (§8 stub-extractor tests)` section):

- `run_turn_batch_n_successes_decode_to_n_results` — 3 `Decl`-kind items,
  all `"ok"`; asserts `results.len() == 3` and each decodes correctly
  through `decode_turn_output_dir`. Stub exits 0.
- `run_turn_batch_mid_batch_failure_attributes_and_stops` — 3 requested
  items, stub reports item 0 `"ok"` and item 1 `"failed"` (item 2 absent
  from the report entirely); asserts `results.len() == 1`,
  `failure.index == 1`, and the attributed `CompileError::Diagnostics`
  carries the real message. Stub exits 1 (proving exit code isn't trusted —
  Ambiguity 1).
- `old_diag_reader_still_parses_batch_stub_stdout_and_sees_failing_item` —
  spawns the SAME §8-shaped stub directly via `ExtractCmd` (not through
  `run_turn_batch`) and asserts the UNMODIFIED `parse_diag_report` still
  parses the document and still surfaces the failing item's real message via
  the flat top-level `diagnostics` array. (A second, pure-parse pin of the
  same property lives in `diag.rs`:
  `old_diag_reader_still_parses_batch_document_and_sees_failing_item_diagnostics`.)
- `run_turn_batch_spawns_extract_exactly_once_with_turn_batch_flag` — mirrors
  `run_turn_spawns_extract_exactly_once_with_turn_flag` (`turn.rs:1487` pre-lane);
  asserts exactly one logged spawn, argv contains `--turn-batch` and
  `--batch-out`, and no bare `--turn` token.

Every fixture item uses a `Decl`-kind verdict deliberately:
`decode_turn_output_dir`'s `Decl` arm never reads `result.cbor`/`meta.cbor`,
so these tests need only a fabricated `TurnOut` CBOR sidecar (reusing the
existing `build_cbor` helper and the same shape `decode_turn_out_decl_variant`
already pins) — no `CoreExpr`/`DataConTable` CBOR fixture had to be
fabricated. `run_turn`'s own `Bind`/`Expr` decode paths (which DO read
`result.cbor`/`meta.cbor`) are already covered by `run_turn`'s existing
tests and are untouched by this lane.

`diag.rs` additionally gained `parse_batch_diag_report_decodes_items_and_flat_diagnostics`,
pinning the new parser's own shape independent of `run_turn_batch`.

## `run_turn` unchanged

`run_turn`'s only change is its tail now calling the extracted
`decode_turn_output_dir(temp.path())` instead of inlining the same code —
same reads, same order, same error types. Confirmed by:
- `run_turn_spawns_extract_exactly_once_with_turn_flag` — still green,
  unmodified.
- `run_turn_missing_template_is_clean_error_not_panic` — still green,
  unmodified.
- `turn_classification_corpus_old_and_new_path_agree` (the GHC-heavy
  classification-equivalence corpus, real extract) — still green.
- All `decode_turn_out_*` tests — still green, unmodified (this function
  wasn't touched; only its caller moved).

## Verify legs run (all green)

```
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo fmt --all -- --check
cargo nextest run -p tidepool-extract-cmd                                    # 10/10 pass
scripts/battery.sh -p tidepool-runtime -E 'test(turn::tests) or test(diag::tests)'
                                                                                # 52/52 pass, incl. the GHC-heavy
                                                                                # classification corpus (36.4s) — no
                                                                                # separate "session_turn" binary
                                                                                # exists; turn.rs/diag.rs tests are
                                                                                # part of the `tidepool-runtime` lib
                                                                                # unit-test binary, scoped via -E.
scripts/battery.sh -p tidepool-runtime -E 'binary(cross_mode_targeted)'       # 10/10 pass — the normal path
                                                                                # (independent of this lane) is
                                                                                # untouched.
```

## Done-criteria check

- `ExtractCmd` has `turn_batch` + `batch_out`; still the only invocation
  builder (its own no-open-coding test still passes). ✅
- `run_turn_batch` is ONE new spawn site and shares `run_turn`'s per-item
  decode via `decode_turn_output_dir`. ✅
- §8 stdout parses (`parse_batch_diag_report`); an exact-match version-1
  reader (`parse_diag_report`, unmodified) still parses the new document and
  still sees a failing item's diagnostics (pinned by test, both a pure-parse
  version in `diag.rs` and a stub-spawn version in `turn.rs`). ✅
- Four stub-extractor tests: N-success, mid-batch failure attribution,
  old-reader compatibility, spawns-exactly-once-with-`--turn-batch`. ✅
- `run_turn` unchanged (behaviorally; its tests are unmodified and green,
  including the GHC-heavy corpus). ✅
- All four verify legs green, `cross_mode_targeted` especially. ✅
