# Sub-TL `boot` — LEDGER

Items **0** (one-compile bootstrap, Track 1) and **0b** (nameable effect
vocabulary). Spec: `00-spec.md`. Wave operational block:
`../OPERATIONAL.md`.

One row per item: decision, receipt counts, fold conflicts.

---

## Anchors read (2026-08-08)

Confirmed against the tree at `be05f291`, so the specs below cite live lines:

| Anchor | Line | What it is |
|---|---|---|
| `tidepool-runtime/src/session/persistent.rs` | 269–271 | `PersistentSession::new` → `machine: None` — the lazy lifecycle already there |
| `tidepool-runtime/src/session/persistent.rs` | 393 | `bootstrap_if_needed` — no-op when already live |
| `tidepool-runtime/src/session/resident.rs` | 208 | `ResidentSession::bootstrap` — the program-shaped constructor (the defect) |
| `tidepool-runtime/src/session/resident.rs` | 220 | its `core.bootstrap_if_needed(expr, &table)` + `seed_session_table` |
| `tidepool-repl/src/session.rs` | 975, 1105, 1257, 1327, 1469, 2002 | the REPL's bootstrap-from-first-real-compile — the pattern to mirror |
| `tidepool-harness/src/harness.rs` | ~430 | answerer boot seed (`pure (toJSON (0 :: Int))`) |
| `tidepool-harness/src/harness.rs` | 790 | `force()` — the ONLY consumer of `self.boot` |
| `tidepool-harness/src/selfharness/driver.rs` | ~552 | outer boot seed (same trivial compile) |
| `tidepool-harness/src/selfharness/driver.rs` | 624 | `compile_outer` — one `compile_turn` per call |
| `tidepool-harness/src/selfharness/driver.rs` | 929, 1507 | the loop and render `compile_outer` calls |
| `tidepool-harness/src/compile.rs` | 104 | `compile_turn` — single-target (`{target}.cbor`) |
| `haskell/app/Main.hs` | 333 | `writeWholeModuleClosed` — one target, one `meta.cbor` |
| `tidepool-mcp/src/eval_prep.rs` | 116 | `effects_module_source_at` — GADTs+helpers emitted ONLY for the row |
| `tidepool-mcp/src/effect_defs.rs` | ~790 | `runLLMTurn :: Member RunLLMTurn effs => …` — ALREADY row-polymorphic |
| `tidepool-codegen/src/jit_machine.rs` | 1940–1956 | `add_function` re-resolves ConTags per fragment table |

### Two findings that shape the work

1. **Step 6 is free.** `self.boot` is consumed at exactly one site,
   `force()` (harness.rs:790). Once `force()` builds an unbootstrapped
   session, the answerer's machine boots on its first real compile — which
   IS the model's first block. No separate work item; it is an assertion in
   `boot-lazy`'s acceptance.
2. **ConTags at a pure boot.** The outer machine would boot from `render`,
   whose expr is pure (`pure (Loaded.render …)`) and may carry no
   RunLLMTurn ConTags. `add_function` re-resolves ConTags against each
   fragment's table (jit_machine.rs ~1940–1956), so a `MissingConTags`
   boot is recoverable when the loop fragment lands; and with step 4's
   merged meta the render table already carries them. Both legs are pinned
   by test, not argued.

---

## Decomposition — 4 dev leaves, 3 waves

| Dev | Item | Scope | Gate |
|---|---|---|---|
| `boot-count` | 0 (receipt) | extract-spawn counter + launch→first-model-call acceptance test; commit the BASELINE (expect 4) | none — wave 1 |
| `boot-vocab` | 0b | vocabulary-vs-row split in `effects_module_source_at`; answerer `Member` negative test | none — wave 1 |
| `boot-lazy` | 0 steps 1–3 (+6) | `ResidentSession::unbootstrapped`, bootstrap-on-first-real-run, DELETE both seeds | **held on parent's driver.rs go-signal** |
| `boot-onecompile` | 0 steps 4–5 | multi-target emission from ONE GHC session; render+loop in one invocation | after `boot-lazy` folds |

Wave 1 runs unconditionally (neither dev touches `driver.rs`). Waves 2 and
3 are sequential: they overlap heavily in `driver.rs` and
`tidepool-harness/src/compile.rs`, so running them in parallel would buy
nothing but conflict.

`boot-count` deliberately lands FIRST so item 0's done-criterion is an A/B
against a committed red-line rather than an after-the-fact assertion.

---

## Item rows

### Item 0 — one-compile bootstrap (Track 1)

- **Fix-ladder rung:** rung 1 (Eliminate) attempted. _Not yet resolved._
- **Blocker (if any downgrade to rung 2):** _none recorded._
- **Receipts:** _pending._

### Item 0b — nameable effect vocabulary

- **Decision:** _pending._
- **Receipts:** _pending._

---

## Fold conflicts

_None yet._ Expected overlap with sub-TL `spawn-latency`:
`haskell/app/Main.hs` (its D1 reworks `writeWholeModuleClosed`'s metadata
merge ~line 348; our step 4 splits the same function's per-target emission)
and `tidepool-runtime/src/session/compile.rs` / `turn.rs`. Per the
realm-spike conflict experiment these are NOT pre-negotiated — minimal
localized diffs, log anything non-mechanical here at fold.
