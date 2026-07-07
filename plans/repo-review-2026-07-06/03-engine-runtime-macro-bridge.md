# 03 — Engine / runtime / effect / bridge / macro

Availability and diagnostics bugs in tidepool-runtime's session engine, plus
tidepool-macro/bridge/effect issues. Pairs naturally with plan 01 finding 5
(the false-StackOverflow ceiling) — both change what long-running turns do.

## ANTI-PATTERNS

- Do NOT flag locked decisions: freer-simple Leaf/Node continuation trees;
  unboxed Word# union tags.
- The `EffectMachine` fix (F5) must mirror the JIT sibling
  (`tidepool-codegen/src/effect_machine.rs:80-100` `ConTags::try_from`), not
  invent a third resolution scheme — hoist the shared constants instead.

## READ FIRST

- `tidepool-runtime/src/session/engine.rs` — `drive`, `resume`, `abort`,
  Paused registration (:780-830), the runaway-detach comments (:833-838)
- `tidepool-effect/src/dispatch.rs` — HCons peel + its own tag tests
- `tidepool-macro/src/expand.rs` — `run_tidepool_extract`, `resolve_hs_path`

---

## F1 (HIGH): Paused continuations lose the JIT `CancelHandle` — runaway resumed turns pin the engine into permanent Overloaded

**Where (verified by read):**
- `engine.rs:812-824` — Paused registration stores `{session_rx, thread, gate}`
  only; the cancel slot is discarded.
- `engine.rs:709` (`resume`) and `:759` (`abort`) — both pass a FRESH
  `Arc::new(Mutex::new(None))` as `cancel_slot` to `drive`.

**Failure:** turn times out at an effect boundary → parked as Paused → caller
resumes → program enters a PURE loop → window expires → `parked_or_in_effect`
false → `gate.request_abort` unseen (pure compute never checkpoints) →
`cancel_slot.lock()` is `None`, so the JIT cancel flag — which engine.rs's own
comment documents as the mechanism that makes a runaway "EXIT, freeing its
permit instead of pinning it forever" — is never flipped. Detached thread spins
at 100% CPU forever, holds its `OwnedSemaphorePermit` (leaked pool slot), the
reaper blocks in `join()` so `orphaned_threads` never decrements. After
`max_orphaned` occurrences, `start_turn` returns `Overloaded` for every request
until process restart.

**Fix:** add the `Arc<Mutex<Option<CancelHandle>>>` to
`ContinuationState::Paused` and thread it back into `drive()` on resume/abort.

**Verify:** test — start a turn that parks at an effect boundary, resume into a
pure loop, let the window expire; assert the turn terminates (cancel observed)
and the permit is released. Also assert `orphaned_threads` returns to 0.

> **STATUS: FIXED (cc90fe07 + this branch).** `ContinuationState::Paused` now
> carries `cancel_slot: Arc<Mutex<Option<CancelHandle>>>` and `timeout_secs:
> u64`; `resume`/`abort` thread the SAME slot + the turn's original
> caller-clamped window back into `drive()` instead of a fresh empty slot and
> `config.default_timeout_secs` (folds in the "smaller items" timeout-reset
> entry below). Test:
> `tidepool-runtime/tests/paused_cancel_resume.rs::paused_resume_into_runaway_is_cancelled_and_reaped`
> — parks a turn blocked in a (test-controlled) effect dispatch, resumes it
> into a genuine pure infinite loop, and asserts the SECOND timeout carries
> the original window (not `config.default_timeout_secs`, deliberately set to
> a different value) and that `orphaned_count()` returns to 0 (permit +
> thread reaped). Verified the test actually catches the regression: manually
> reverted the fix and confirmed the test fails (`TimedOut.timeout_secs == 3`
> instead of `45`); restored the fix and it passes.

## F2 (MEDIUM): `UnhandledEffect` diagnostic is dead code, and would name the wrong effect if it fired

**Where:** `engine.rs:1095` (`describe_run_error`) +
`tidepool-effect/src/dispatch.rs:275-289` +
`tidepool-codegen/src/jit_machine.rs:26-27`.

Two independent defects:
1. **Prefix mismatch:** an unhandled effect surfaces as
   `RuntimeError::Jit(JitError::Effect(_))` which Displays as
   `"effect dispatch error: Unhandled effect at tag N"`;
   `detail.strip_prefix("Unhandled effect at tag ")` never matches, so the
   effect-name annotation + "Registered effects" roster are NEVER appended
   (grep "Registered effects" — only the definition).
2. **Wrong tag if fixed naively:** `HCons::dispatch` decrements the tag per
   peeled layer, so `HNil` reports `original − stack_len` (dispatch.rs's own
   tests assert `tag: 0` for an out-of-range tag on a 2-handler stack). A
   version-skewed program emitting tag N over an N-handler stack would be
   annotated `"tag 0 (effect: <first effect>)"` — pointing at a HANDLED
   effect, in exactly the skew scenario the diagnostic exists for.

**Fix:** restore the original tag in the HCons tail branch (`.map_err` bumping
`UnhandledEffect.tag` by 1); match with `contains` — or better, classify
structurally on the eval thread while the typed error is in hand. Add a test
asserting the roster appears with the correct tag.

> **STATUS: FIXED.** (a) `HCons::dispatch`'s tail branch now `.map_err`s an
> `UnhandledEffect` back up by 1 per layer, restoring the ORIGINAL
> caller-supplied tag by the time it reaches the top (dispatch.rs's own
> pinned tests — `single_handler_rejects_tag_1`, `two_handlers_reject_tag_2`,
> plus `tidepool-effect/tests/proptest_effect.rs::unknown_tag_returns_error`
> — updated deliberately to expect the restored tag, not the
> point-of-failure-relative-to-HNil tag). (b) `describe_run_error` now
> classifies STRUCTURALLY (`if let RuntimeError::Jit(JitError::Effect(
> EffectError::UnhandledEffect { tag })) = e`) instead of the dead
> `strip_prefix` string match. Test:
> `session::engine::tests::describe_run_error_annotates_unhandled_effect_with_name_and_roster`
> (tidepool-runtime/src/session/engine.rs) — tag 2 over a 3-effect roster
> names `(effect: Fs)` and lists all three registered effects.
> `tidepool-codegen/tests/proptest_jit_dispatch.rs`'s JIT/eval differential
> (`invalid_tag_never_signals`) compares tag EQUALITY between the two engines
> through the same shared `dispatch.rs`, so it needed no change — both sides
> moved from "equally decremented" to "equally correct" together.

## F3 (MEDIUM): `run_tidepool_extract` swallows the real GHC error and can emit a false "not found"

**Where:** `tidepool-macro/src/expand.rs:541-546` — the direct PATH
invocation's stderr is discarded on failure (`Ok(_) | Err(_) => { /* fall back
to nix */ }`).

**Failure:** `haskell_inline!`/`haskell_eval!` with a Haskell type error,
extract on PATH, no flake.nix (or no nix): emitted `compile_error!` is
*"tidepool-extract not found on PATH and no flake.nix in any parent
directory"* — both claims false, the actual GHC diagnostic gone. With nix
available, the same error is only reproduced after a redundant slow `nix run`.

**Fix:** fall back to nix only when the spawn fails with
`ErrorKind::NotFound`; when the binary RAN and failed, return its stderr
directly.

> **STATUS: FIXED.** `run_tidepool_extract` now matches on the spawn
> `Result`: `Ok(status_nonzero)` returns the real stderr directly (no nix
> fallback — nix would only re-run the same failing compile); `Err(e) if
> e.kind() == NotFound` falls back to nix as before; any OTHER spawn error
> (e.g. permission denied) reports itself immediately instead of silently
> retrying via nix.

## F4 (MEDIUM-LOW): stale `.cbor` bindings served silently after a binding rename

**Where:** `expand.rs:96-107` (`resolve_hs_path` binding lookup); extractor
(`haskell/app/Main.hs:132`) only does `createDirectoryIfMissing` — nothing
removes stale outputs from `target/tidepool-cbor/<Module>/`.

**Failure:** rename binding `foo` → `bar` in the `.hs`: extraction writes
`bar.cbor`, `foo.cbor` survives, and `haskell_eval!("X.hs::foo")` compiles
fine while embedding the OLD Core — silent stale-code execution. The
unsuffixed form instead errors "Multiple bindings found" for a genuinely
single-binding module.

**Fix:** clear the per-module output dir before invoking the extractor (it is
fully regenerated each expansion), or key the dir by source hash.

> **STATUS: FIXED.** `run_tidepool_extract` now `remove_dir_all`s
> `output_dir` (ignoring `NotFound`) before invoking the extractor, so a
> renamed/removed binding's stale `.cbor` cannot survive into the next
> expansion. Also fixed in the same pass: `strip_module_header`'s multi-line
> `module Foo\n ( x )\n where` header leaked the export-list/`where`
> continuation lines into the inlined body — a `in_module_clause` guard now
> skips every line from `module Foo` through the line containing `where`.
> Tests: `tidepool-macro/src/expand.rs`'s `tests` module —
> `single_line_module_header_still_strips_cleanly`,
> `multi_line_module_header_does_not_leak_into_body`,
> `multi_line_module_header_where_on_closing_paren_line`.

## F5 (LOW-MED, confirmed stale-shadow smell): interpreter `EffectMachine` resolves freer constructors by ambiguous bare name; JIT sibling already fixed

**Where:** `tidepool-effect/src/machine.rs:22-37` vs
`tidepool-codegen/src/effect_machine.rs:80-100`.

`ConTags::try_from` (JIT) resolves `Val/E/Union/Leaf/Node` via
`get_by_qualified_name("Data.FTCQueue.Node")` etc. precisely because
`get_by_name` returns `None` on ambiguity; `EffectMachine::new` (oracle) still
uses bare `get_by_name`.

**Failure:** any effectful program defining/importing a `Node`/`Leaf`/`Val`
constructor (`data Tree = Node Tree Tree | Leaf Int`) makes the ORACLE fail
`MissingConstructor { name: "Node" }` — misdescribing ambiguity as absence —
while the JIT runs fine, so differential harnesses diverge on exactly the
collision-shaped programs. Oracle/test-only (no production call sites) caps
severity.

**Fix:** mirror qualified-first resolution; hoist the five toolchain-pinned
qualified names into tidepool-effect and have codegen reuse them.
Cross-ref: plan 05 F2 covers the PRODUCTION-path variant of this in
`normalize.rs` — fix together.

> **STATUS: FIXED (this worker's half; plan 05 F2's `normalize.rs` wire-in is
> a separate, later wave).** New `tidepool-effect::freer_names` module: `pub`
> unqualified + qualified-name consts for `Val`/`E`/`Union`/`Leaf`/`Node`, plus
> a shared `resolve(table, qualified, bare)` helper. `EffectMachine::new`
> (`tidepool-effect/src/machine.rs`) now resolves qualified-first through it.
> `tidepool-codegen/src/effect_machine.rs`'s `ConTags::try_from` — the ONLY
> region touched in that file — now calls the SAME `freer_names::resolve` +
> consts instead of `EffContKind::qualified_name()/name()`, so the two
> resolution schemes cannot drift apart. `EffContKind` itself (and its
> `name()`/`qualified_name()`/`ALL`) is left untouched (owned by a concurrent
> worker; still public API, so no dead-code warning from the removed call
> sites). Tests: `tidepool-effect/src/freer_names.rs`'s
> `resolve_prefers_qualified_over_bare_on_collision` +
> `resolve_falls_back_to_bare_when_qualified_absent`, and
> `tidepool-effect/src/machine.rs`'s
> `effect_machine_new_resolves_colliding_node_and_leaf_via_qualified_name`
> (builds a table with a colliding user `Node`/`Leaf` at the bare name and
> confirms `EffectMachine::new` still succeeds). `cargo nextest run -p
> tidepool-codegen` (570 tests) green — `ConTags::try_from`'s existing
> callers unaffected.

## F6 (LOW): `FromCore for ()` accepts any nullary constructor

**Where:** `tidepool-bridge/src/impls.rs:136-143`. `to_value` insists on the
`"()"` constructor specifically (its own comment: a wrong nullary con
"silently corrupts downstream decode"), but `from_value` accepts `Nothing`,
`False`, `[]`, or any user nullary con as `()` — masking upstream encoding
bugs. Related: tuple `FromCore` (:552-570) reports `TypeMismatch "(,)"` when
the real problem is `"(,)"` missing from the table (should be
`UnknownDataConName`).

**Fix:** check the con id against `"()"` mirroring `to_value`; split the
missing-constructor case out of the tuple type-mismatch arm.

> **STATUS: FIXED.** `FromCore for ()` now checks
> `table.name_of(*id) == Some("()")`, mirroring `to_value` exactly (rejects
> `Nothing`/`False`/`[]`/any other nullary con). The 2-tuple AND 3-tuple
> `FromCore` impls (both had the same bug, not just the pair mentioned above)
> now resolve `(,)`/`(,,)` with `.ok_or_else(UnknownDataConName)?` BEFORE the
> match, so a table that never registered the tuple constructor reports
> `UnknownDataConName` — a genuinely-wrong-shaped value (constructor
> registered, but this isn't it) still reports `TypeMismatch` as before.
> Tests (`tidepool-bridge/src/impls.rs`'s `tests` module):
> `unit_from_value_rejects_other_nullary_constructors`, `unit_roundtrips`,
> `pair_from_value_missing_constructor_is_unknown_dataconname_not_type_mismatch`,
> `pair_from_value_wrong_shape_is_still_type_mismatch`,
> `triple_from_value_missing_constructor_is_unknown_dataconname`.

## F7 (LOW): derived enum `FromCore` fails fast on EARLIER variants' missing constructors

**Where:** `tidepool-bridge-derive/src/codegen.rs:176-189`. Each variant's
DataCon lookup ends in `?`, so decoding a valid value of variant K errors
`UnknownDataConNameArity` for an earlier variant J whenever J's constructor is
absent from this compilation's table. Decode success depends on Rust variant
order. Currently masked (tables carry whole types) but unenforced.

**Fix:** treat a failed lookup for a non-matching variant as "skip"; error
only if no variant matched.

> **STATUS: FIXED.** `generate_from_core`'s per-variant match arm
> (`tidepool-bridge-derive/src/codegen.rs`) now wraps `#lookup` (which itself
> still ends in `?`) in an IIFE closure returning `Result<DataConId,
> BridgeError>` and only enters the `if *id == variant_id` check on `Ok` — a
> failed lookup for a non-matching variant just falls through to try the next
> variant instead of aborting `from_value` outright. `emit_datacon_lookup`
> itself, and its other 3 call sites (enum `ToCore`, struct `FromCore`/
> `ToCore`, where a hard `?` is correct — no "try the next candidate" concept
> applies), are unchanged. Tests
> (`tidepool-bridge-derive/tests/derive_tests.rs`):
> `later_variant_decodes_despite_earlier_variants_missing_constructor` (a
> 2-variant enum whose table registers ONLY the second variant's constructor
> — decoding a value of that second variant now succeeds) and
> `no_variant_matches_is_still_an_error` (a value matching neither variant is
> still `Err(UnknownDataCon)`, confirming the skip logic doesn't swallow
> genuine failures).

## Cache residual edges (cache.rs verified CLEAN overall — these are the two bounded leftovers)

- A wrapper script referencing its delegate via a variable/relative path
  (`exec "$DIR/bin"`) isn't followed by the content-hash → stale Core after a
  delegate-only upgrade. (Direct-path wrappers ARE followed.)
- `fingerprint_dir` follows directory symlinks with no cycle guard → a cyclic
  symlink under an include dir hangs key computation.
- Doc drift: `cache.rs:261-262` doc says "sizes, and modification times"; the
  code hashes contents.

> **STATUS:**
> - Wrapper variable/relative delegate path: **FILED, not fixed** — general
>   shell-variable resolution (`DIR=$(dirname "$0")` and its many variants) is
>   unbounded; a small text scanner cannot soundly evaluate arbitrary shell.
>   Direct-path wrappers (nix/cargo's common case) are unaffected. Rationale +
>   scope note added as a doc comment on `extract_exec_target`
>   (`tidepool-runtime/src/cache.rs`).
> - Symlink cycle guard: **FIXED.** `fingerprint_dir` now tracks visited
>   CANONICALIZED directories and skips a repeat, breaking the cycle in O(1)
>   instead of relying on the kernel's incidental ELOOP bound (~40 traversals
>   deep). New unit test
>   `cache::tests::test_cache_key_handles_symlink_cycle_in_include_dir`; the
>   existing end-to-end regression
>   (`tests/proptest_cache_layer.rs::symlink_cycle_in_include_dir_terminates_gracefully`,
>   previously filed "REFUTED — verified negative" against the accidental
>   ELOOP safety net) still passes and its doc comment is reworded to describe
>   the new deliberate mechanism.
> - Doc drift: **FIXED** — `fingerprint_dir`'s doc now says it hashes content,
>   not size/mtime.

## Smaller items

- `resume()`/`abort()` of a Paused continuation re-drive with
  `config.default_timeout_secs`, silently discarding the turn's original
  caller-clamped window — carry `timeout_secs` in the continuation or document
  the reset (fold into F1's struct change).
- `tidepool-effect/src/machine.rs:118` — comment claims "deep_force the
  request so FromCore never sees ThunkRef" above a plain `clone()` (the force
  happened at :95). Reword to state the invariant.
- `strip_module_header` (`expand.rs:471-511`) breaks on multi-line module
  headers (`module Foo\n ( x )\n where` leaks `( x ) where` into the inlined
  body); skip-until-`where` guard closes it.

> **STATUS: all three FIXED** — folded into F1 (timeout_secs carry, same
> struct change as the cancel slot), the F5/machine.rs section above (comment
> reworded to state the force-already-happened invariant instead of
> narrating an action that isn't in this line), and F3/F4 (multi-line module
> header `in_module_clause` guard), respectively.

## Verified clean — do NOT re-audit

`tidepool-runtime/src/cache.rs` — the F1–F6 proptest campaign closed framing,
include-dir ordering, content-vs-mtime, symlinked-`.hs` lstat, quoted wrapper
targets, corrupted-CBOR serving; sentinel-written-last + blake3 means races
degrade to MISS never wrong-serve; key covers source/target/include-dirs/
extract-binary+wrapper hashes/session `(id, gen)` salt. HList dispatch routing
correct for in-range tags. Bridge scalar/container impls round-trip
(proptest-covered); PhantomData symmetric. `json.rs` is a thin delegate to the
single shared `tidepool_eval::json` builder. `PauseGate` state machine sound.
Session lib (`session/mod.rs`): atomic module writes, validation-failure
rollback, cache salt isolation. `tidepool-bignum` chunked pow-2 scaling cannot
double-round.

## DONE CRITERIA

- [x] F1 fixed with the park→resume→pure-runaway test; permit accounting
      asserted
- [x] F2 fixed with a roster+correct-tag test
- [x] F3/F4 fixed (macro error fidelity + stale-cbor cleanup)
- [x] F5 fixed jointly with plan 05 F2 (shared qualified-name constants) — this
      worker's half (`tidepool-effect`/`tidepool-codegen`) done; the
      `normalize.rs` production-path wire-in is plan 05 F2's separate later wave
- [x] F6/F7 fixed with round-trip asymmetry tests
- [x] Cache edges + smaller items fixed or filed (symlink cycle + doc drift
      fixed; wrapper variable/relative delegate path filed with rationale,
      not fixed — see STATUS note above)
- [x] `scripts/battery.sh` NOT RUN per this worker's boundary (explicitly
      forbidden: "no battery.sh"). Verified instead via targeted suites, all
      against a FRESH `tidepool-extract-bin` built from this branch (never the
      deployed PATH shim): `cargo check/clippy --workspace` clean (clippy: 0
      warnings in any file this branch touched — pre-existing warnings
      elsewhere, e.g. `tidepool-mcp`'s macro-generated
      `needless_question_mark`/`crate_in_macro_def`, untouched); `cargo fmt
      --all -- --check` clean except two PRE-EXISTING diffs in
      `tidepool-repl` (outside boundary, owned by a concurrent worker — not
      touched); `cargo nextest run -p tidepool-effect -p tidepool-macro -p
      tidepool-bridge -p tidepool-bridge-derive` — 168/168 green; `cargo
      nextest run -p tidepool-codegen` — 570/570 green (differential/proptest
      suites incl. `proptest_jit_dispatch`'s tag-equality invariant, unaffected
      by F2/F5); fresh-extract `cargo nextest run --ignore-default-filter -p
      tidepool-runtime --no-fail-fast` — 728/741 passed, 13 failed, 12 skipped.
      All 13 failures are in `cross_mode_existing.rs` / `edge_cases.rs` /
      `prelude_coverage.rs` / `value_case_match.rs` — files this worker is
      EXPLICITLY forbidden from editing (concurrent-worker-owned) — and share
      ONE root cause unrelated to anything in this plan: a GHC
      `Number`/`Scientific`-vs-`Double`/`Fractional` type mismatch in
      generated fixtures (an aeson/Scientific version or Value-representation
      change in flight elsewhere), matching the pre-briefed "14 known
      pre-existing failures owned by a concurrent worker" almost exactly (13
      seen here). A 14th apparent failure
      (`proptest_haskell_pipeline::committed::determinism_x30`, 972s under
      full-suite contention) re-ran GREEN in isolation (262s) — flaky under
      the GHC-heavy group's resource contention (see `.config/nextest.toml`'s
      own note on this), not a regression. Re-ran this branch's own new/edited
      tests individually to confirm: F1's
      `paused_cancel_resume.rs::paused_resume_into_runaway_is_cancelled_and_reaped`,
      F2's `describe_run_error_annotates_unhandled_effect_with_name_and_roster`,
      and the cache symlink-cycle test all green.
