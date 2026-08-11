# Turn-latency lane plan — wave-3 (render+loop fusion) and D2 (`RuntimeTypeClosure`)

TL: `spawn-latency`, 2026-08-11, branch `root.spawn-latency`, base `fc94ac3b`.

Both items were CUT from the extract-wave for time and routed forward
(`extract-wave.md` CLOSING STATE, "Cut, and routed forward ready-to-spawn").
This plan re-validates each handoff claim against HEAD, maps each item's file
surface against the in-flight sibling lane (`batch-turns`), and fixes the
sequencing.

---

## 0. Sequencing decision (the headline)

**Wave-3 runs FIRST and alone. D2 is BLOCKED on the sibling and does not
start in this lane until `batch-turns` folds.**

The sibling `batch-turns` lane owns, in flight:

    haskell/src/Tidepool/GhcPipeline.hs
    haskell/app/Main.hs
    tidepool-runtime/src/session/turn.rs
    tidepool-repl/src/session.rs
    tidepool-extract-cmd/

File surface, mapped item by item against that list:

| item | files it must edit (verified at HEAD) | overlap |
|---|---|---|
| **wave-3** | `tidepool-harness/src/selfharness/driver.rs`, `tidepool-harness/tests/acceptance_boot_compile_count.rs`, `tidepool-mcp/src/eval_prep.rs` (additive template field) | **none** |
| **D2** | `haskell/app/Main.hs` (**the `allMeta` assembly is the whole edit**), `haskell/src/Tidepool/Translate.hs` | **`haskell/app/Main.hs` — direct hit** |

D2's overlap is not incidental: `allMeta` — the exact expression D2 replaces —
is assembled at `Main.hs:406-416` (`processFile`) and `Main.hs:580-605`
(`writeClosedTargets`), and `writeClosedTargets` is the single write path the
sibling's batch-compile feature also reaches. There is no version of D2 that
does not edit that function. So D2 waits.

Wave-3 needs **zero** Haskell-side change (see §2), which is what makes it the
free item.

---

## 1. D2 — handoff claims re-validated against HEAD

The handoff (`03-d2-handoff.md`) is 2026-08-09 and predates the unified
`runCompile` skeleton, `PipelineVariant`/`TierPolicy`, `ExtractCmd`, the
compile memo, and `--targets`. Checked line by line; what survives and what
moved:

**HOLDS.**

- The metadata source is still unfiltered. `Main.hs:406`/`580`
  `tyconMeta = collectDataCons tycons` over `prTyCons` — and `prTyCons` is
  still every module's own `mg_tcs` with no reachability filter
  (`GhcPipeline.hs:455`, `prTyCons = allTyCons`). The item's premise is intact.
- `collectTransitiveDCons` (`Translate.hs:1291`) still seeds from binder
  `idType`s and expands via `closeTyCons`/`dataConOrigArgTys`
  (`Translate.hs:1302-1321`). The handoff's **traced** supplier route for the
  five freer scaffolding constructors is still structurally present.
- `ConTags::try_from` still hard-requires all five
  (`tidepool-codegen/src/effect_machine.rs:190`), and it resolves them through
  `tidepool_effect::freer_names` (`effect_machine.rs:201-228`). The handoff's
  "DERIVE from `freer_names`, never hand-list" requirement is therefore
  directly implementable — `freer_names` exists as a real shared module with
  `VAL`/`VAL_QUALIFIED`/`E`/`UNION`/`LEAF`/`NODE` consts and a `resolve`
  helper. That is the single source, and it is the one D2 must reach for.
- CHECK A (`assertMetaCoversEmitted`, `Main.hs:707`) is live and is called from
  `writeClosedTargets` (`Main.hs:629`), per target, against the MERGED
  metadata, before the write and before the encode `timeSection`. D2's primary
  detector is in place. Its documented limit still applies verbatim: it guards
  EMITTED constructors only.
- The three pinned id-stability tests still exist by the paths the handoff
  enumerates, and are still all DataConId qualified-name guards.

**MOVED — the handoff must be adjusted here.**

1. **`writeWholeModuleClosed` is no longer the single write path; there are now
   two metadata-assembly sites plus a third caller.** `--targets` landed
   (`boot-targets`): `writeClosedTargets` (`Main.hs:545`) is the multi-target
   write path, `writeWholeModuleClosed` (`Main.hs:~670`) is now a thin
   single-target wrapper over it, and `processFile`'s `--all-closed` path keeps
   its OWN `allMeta` at `Main.hs:406-416`. **The handoff's "ONE EMISSION PATH"
   constraint is still the right rule but now names the wrong function** — the
   one path is `writeClosedTargets`, and `processFile`'s separate assembly is a
   deliberate second site with NO CHECK A (`Main.hs` site comment). D2 must
   narrow **both** or state explicitly why it narrows only the CHECK-A-guarded
   one. Narrowing the un-guarded `processFile` site without its own detector is
   the riskier half and should be sequenced second, or left out with a written
   reason.
2. **`meta.cbor` is now SHARED ACROSS TARGETS.** `writeClosedTargets` merges
   metadata once for every requested target. A reachability closure computed
   per target and then merged is not the same set as one computed over the
   union — D2 must state which it computes and prove the per-target CHECK A
   still passes against the merged table. This hazard did not exist when the
   handoff was written.
3. **`isTypeMetadataVar`** (handoff §4, the ambiguous `kind=4` cause) is at
   `Translate.hs:3009` and still matches on occurrence-name prefix. The root
   dev's fix the handoff says "was in flight" is **not** in this tree at
   `fc94ac3b`. So the handoff's instruction — check `isTypeMetadataVar`'s state
   at your base before attributing a `kind=4` — resolves to: *the prefix-only
   matcher is still there, so cause (2) is live and D2 must not assume a
   `kind=4` is its own fault.*
4. **The gate set is unchanged in substance but one leg got cheaper to state.**
   `haskell_suite_differential` and `corpus_report` still never invoke the
   extractor (standing hazard 1); they remain non-coverage for D2. The honest
   set is still CHECK A + `extract-fidelity-test` + harness acceptance.
5. **The compile memo now exists** (`plans/compile-memo.md`). Any D2
   before/after that counts extract cost must pin its own memo dir
   (`support::isolate_compile_memo`) or it measures cache state. The handoff
   predates this entirely.

**Standing hazard 6 applies to D2 directly.** "A spec can ask for a measurement
no instrumentation could produce." D2's win is CBOR/table SIZE and downstream
JIT work, not necessarily a phase-line delta — `allMeta` is forced by the
UNTIMED `assertMetaCoversEmitted` call, exactly the trap D1-B hit. D2's receipt
must be sized against something actually bracketed: `meta.cbor` byte count and
entry count (both already printed at `Main.hs:649`), the emitted Cranelift
function/block counts, and `cbor_encode`'s own phase line — not a
`translate`-phase delta.

**D2's first step remains unchanged and un-done:** dump a `meta.cbor` for a
PURE entry term and attribute the five freer constructors to a source
empirically. Bank the answer either way.

---

## 2. Wave-3 — handoff claims re-validated against HEAD

The wave-3 spec (`extract-wave/boot/02-wave3-one-compile.md`) already carries
one premise correction. It needs a second, and it is a simplification.

**The entire "extract side" of the spec is DONE.** `--targets` landed on both
sides:

- Haskell: `Main.hs:200-247` parses `--targets a,b`; `runMultiTargetClosed` →
  `writeClosedTargets` emits one `<target>.cbor` per target over ONE merged
  `meta.cbor`, with per-target `<target>.asks.json` when `len > 1` and the
  plain `asks.json` when `len == 1`.
- Rust: `compile::compile_turns` (`tidepool-harness/src/compile.rs:197`)
  drives it in ONE spawn and returns one `CompiledTurn` per target sharing the
  merged table; `compile_turn` is now a thin `targets.len()==1` wrapper over
  it (`compile.rs:148-160`), so the single-target contract is preserved by
  construction rather than by a parallel implementation.

**So wave-3 reduces to exactly what its own premise-correction note predicted:
driver-side fusion only. No Haskell edit. No `compile.rs` edit.**

**A second stale claim, in the spec's soundness argument.** The spec says
"render additionally splices `__selfHarnessCompaction`". At HEAD it does not:
`render_framing` (`driver.rs:1665`) splices only
`state_cross::state_in(state_json)`, and the compaction summary is appended in
**Rust** after the render run (`driver.rs:1697-1702`). Verified: in
`run_one_cycle`, `render_framing(prior_state, …)` (`driver.rs:733`) and
`run_loop_fragment_inner` (`driver.rs:1040`) call `state_cross::state_in` with
the SAME `prior_state`, producing **byte-identical** helper text. The fusion is
therefore *more* obviously sound than the spec argued, not less — one splice,
not two.

**The one real structural obstacle, which the spec does not mention.** The
turn template emits exactly one entry binder, hard-coded:
`result :: Eff <stack> Value` (`tidepool-mcp/src/eval_prep.rs`,
`TurnTemplate::render`). Both `render_framing` and `run_loop_fragment_inner`
compile to target `"result"`. A fused module needs TWO distinct top-level
binders to hand `--targets`.

Two ways to get the second binder, and the choice matters:

- (a) Splice a hand-written second entry through the existing `helpers` slot.
  Localized, no `tidepool-mcp` edit — but it hand-copies the `result` binder's
  shape (`_r <- __user; pure (toJSON _r)`, the budget/anchor conditionals) into
  the driver, where it can drift from the template it is imitating. That is the
  same hand-maintained-copy mechanism this wave's own ledger keeps catching.
- (b) **CHOSEN.** Give `TurnTemplate` an optional extra-entry list rendered
  through the SAME code path that renders `result`, so the two entries are
  identical by construction. Additive (`..Default::default()`), so every
  existing caller — including the sibling-owned `tidepool-repl` and
  `tidepool-runtime` call sites — is untouched and does not conflict.

`render_framing` stays `pub` and keeps working standalone (five acceptance
suites call `run_one_cycle`, and the post-loop render genuinely cannot fuse —
it uses the NEW state). The fused path is additive: `run_one_cycle` gains a
one-compile entry; `render_framing`'s own compile survives for the post-loop
render and for direct callers.

---

## 3. Measurement plan — the instrument, named

`scripts/bench-turn.sh` does **not exist** in this tree at `fc94ac3b` (a
sibling dev is building it). If it lands before the measurement step it becomes
the instrument; otherwise the receipt uses the two instruments below, by hand,
with the method stated.

**Instrument 1 — spawn count (exact, not a timing estimate).**
`tidepool_extract_cmd::extract_spawn_count`, asserted by
`tidepool-harness/tests/acceptance_boot_compile_count.rs` against
`PRE_MODEL_EXTRACT_COMPILES`, currently **2** (measured 2026-08-09 on the
centralized tip, per that constant's own doc). Wave-3's win condition is **1**.
That test already isolates the compile memo (`support::isolate_compile_memo`),
so the count is about the boot path and not cache state. This is a COUNT
receipt, not a latency receipt.

**Instrument 2 — wall clock on the path the item claims to speed.** The same
binary, timed. `acceptance_boot_compile_count` drives launch → first model call
with a cold memo, which IS the pre-model boot path, so its wall time is the
quantity wave-3 reduces. Method, to be stated verbatim in the receipt:

    export XDG_CACHE_HOME="$PWD/.cache"
    scripts/battery.sh -p tidepool-harness -E 'binary(acceptance_boot_compile_count)'

run **N=3 per side**, reporting each run and the median, A/B via
**diff-to-patch** (`git diff > /tmp/…patch` → `git apply -R` → measure → `git
apply`) — never `git stash`. Each run gets a fresh memo (the test isolates it
itself). Report the raw numbers; do not average away a bimodal result.

**Instrument 3, if `TIDEPOOL_TIMING=1` phase lines are wanted alongside:** the
`extract.*` phase lines the compile emits per spawn. Two spawns' phase lines
before, one after — this attributes the saving to GHC rather than to Rust, and
it is the honest way to say *where* the time went. Optional; instruments 1+2
are the required pair.

**What NOT to claim.** The saving is one whole GHC extract compile on the
pre-model path. It is not "half the turn latency": the first cycle pays two
compiles today, later cycles pay a post-loop render too, and `run_one_cycle`'s
cost is dominated by model calls once the loop is running. State the measured
delta on the measured path and nothing wider.

---

## 4. Execution

1. Wave-3, spawned as a dev leaf against this plan, file boundary
   `tidepool-harness/src/selfharness/`, `tidepool-harness/tests/`,
   `tidepool-mcp/src/eval_prep.rs`.
2. Measured receipt (§3), fold-quality verification, handoff doc stamped.
3. D2: not started. If `batch-turns` folds inside this lane's life, pick it up
   against §1's adjusted claims. Otherwise it hands up as
   blocked-on-sibling with §1 as its ready-to-spawn spec — which is strictly
   more than it had at hand-off, since every claim in it is now re-validated
   against HEAD.

## 5. Verify legs (which apply, given the surface)

Wave-3 touches no Haskell and no extract behaviour, so the extract-side legs
(`extract-fidelity-test`, `cross_mode_targeted`) are **not** triggered by it —
stating this rather than running them for show. What applies:

    cargo check --workspace --all-targets
    cargo clippy --workspace --all-targets
    cargo fmt --all -- --check
    cargo nextest run
    export XDG_CACHE_HOME="$PWD/.cache"
    scripts/battery.sh -p tidepool-harness -E 'binary(golden_path) + binary(acceptance_askuser)'
    scripts/battery.sh -p tidepool-harness -E 'binary(acceptance_boot_compile_count)'

plus `binary(acceptance_multi_target)` — the `--targets` guard wave-3 now leans
on — and `binary(selfharness_spine)`. One GHC leg at a time.
