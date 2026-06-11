# Proptest Bug-Hunting Campaign — Consolidated Report

Date: 2026-06-10/11. Two waves, 14 workstreams, 12 findings ledgers
(`plans/proptest-findings-*.md`). Success metric: distinct confirmed findings
with committed seeds + repros. **Total: 24 findings — 9 fixed in-session, 1
structural defense landed, 1 reclassified, 13 in the fix backlog.**

## Wave 1 — full-machinery hunting (8 workstreams)

| WS | Target | Outcome |
|----|--------|---------|
| W1 foundation-depth | depth-3 generator cap | Cap lifted (worklist comparison, depth/weight params); 2 divergences triaged as non-bugs; `jit_only_error` now fails loud; floor 50→120 |
| W2 ghc-idioms | letrec phases / joins / boxing | 1 finding (jump-crossing-Lam precondition, error now names the invariant); **verified negative** at 100% comparison reach over the 3 highest-density historical subsystems |
| W3 boundary-roundtrip | value_to_heap / FromCore | 2 bugs → **fixed**: Text off+len overflow panic→clean Err; u16 Con-field silent truncation→TooManyFields |
| W4 lazy-consumption | #313's home turf | **Lazy machinery exonerated** (33-cell matrix, 0 divergences); #313 reproduced + localized to cross-module unfolding |
| W5 jit-dispatch | dispatch loop, tag routing | 1 bug → **fixed**: shape-mismatched resume returned pointer garbage; unbox loops now shape-guarded (Con arity + lit-tag), strict contract live |
| W6 haskell-pipeline | Translate.hs via source fuzzing | **Verified negative**: 1400 long-haul cases, 43.8% gotcha-shape coverage, 0 failures (pure path) |
| W7 host-arrays | ByteArray/CAS/double host fns | 2 bugs → **fixed**: show-double scientific mantissa; resize dealloc-layout UB (capacity-word ABI) |
| W8 render-json | value_to_json | 3 bugs → 2 **fixed** (off-clamp panic, exactly-at-cap truncation fencepost), 1 reclassified (empty-`[Char]` = erasure semantics, pinned) |

### The headline: #313 CLOSED
W4's localization (lib-module `t7` traps, byte-identical inline is clean, both
lazy modes identical) → trap decode (`(,,)` tuple at a `[]/(:)` case) →
`--dump-core` diff → **two distinct top-level floats both named `k_X1`**:
`Translate.localVarId` hashes (occName, unique-key), unique keys are
per-module, multi-module bind concatenation collides them, the resume went
through the wrong continuation. Fix: `GhcPipeline.externalizeInternalTops`
(commit 2d0ca80). Repro is now the active `regression_313_lib_t7`.

## Wave 3 — subcomponent audit (6 workstreams; 2 still converging)

| WS | Target | Outcome |
|----|--------|---------|
| S2 freer-queue | the eval oracle's continuation tree | 2 bugs: **B3 recursive queue walk** (deep queues kill the oracle thread), lax Val arity. Laws (associativity, threading, qComp) hold when stack suffices |
| S4 oracle-the-oracle | test infra itself | 4 bugs: **BUG-1 comparator equated Lit-vs-Con** (every differential green was shape-class-weak; fixed in-session), BUG-2 ByteArray reflexivity (fixed), BUG-3 depth-cap overshoot ~d, BUG-4 (Err,Ok) misclassification. Trust verdict: fired verdicts sound; greens weaker than advertised in exactly the Lit-vs-Con dimension |
| S5 cache-layer | compile cache | 8 findings (1 refuted): **F2 High** include-order key sharing (wrong artifact served), **F3a High** content-blind binary fingerprint (nix mtime normalization), F1 NUL framing collisions, F3b/F4/F5 staleness routes (same-size edits, lstat symlinks, quoted wrapper exec), F6 no payload integrity. Verdict: binary IS fingerprinted (cache-clear ritual mostly obsolete) but three stale routes remain |
| S6 varid-defense | #313 class closure | **Detector landed**: duplicate top-level VarIds fail loudly at compile entry (kill-switch `TIDEPOOL_VARID_CHECK=0`), corpus-calibrated, 56-bit birthday-bound analysis. No wild duplicates found |
| S3 heap-layout | raw layout primitives | 4 bugs + 3 verified-negatives: **C2 Critical** — `alloc_con` (effect_machine.rs:721) has NO field-count guard; `24+8*len as u16` wraps at len≥8189 while NUM_FIELDS stays correct → GC `evacuate` copies the truncated size (8189-field Con evacuates as 0 bytes, fields lost) and the cheney scan walks into garbage. C1 raw `write_header` u16 truncation; C3 lit-tag constant drift (codegen tags 5-9 unknown to tidepool-heap's `LitTag::from_byte`); C6 BLACKHOLE thunks report 0 pointer fields → captures invisible to GC mid-evaluation. Verified: capacity-word ABI invariants (C5), NaN bit-identity (C7) |
| S1 parked-registry | (converging) | — |

## Fixes landed this session (commits)

`2d0ca80` #313 VarId collision · `ed7baa1` render trio + bridge pair ·
`bb0bc09` show-double + resize-UB capacity-word ABI · `f137d34` unbox shape
guards + join-error invariant · `bebcf49` W6 over-cap whitelist · plus
comparator fixes (BUG-1/BUG-2, pending verification at time of writing) ·
S6's load-time VarId detector (merged).

## Fix backlog (prioritized)

| P | Item | Source |
|---|------|--------|
| P0 | Strengthened-oracle differential re-run: any previously-masked Lit-vs-Con divergence is a real JIT bug | S4 BUG-1 follow-through |
| P1 | `alloc_con` field-count guard (mirror heap_bridge's MAX_FIELDS) + raw `write_header` size guard — GC-corruption class | S3 C2 (Critical) / C1 |
| P1 | BLACKHOLE pointer-field visibility in `for_each_pointer_field` (captures invisible to GC mid-force) | S3 C6 |
| P1 | EffectMachine iterative queue walk | S2-B3; recursion-sweep slice 1 |
| P2 | Lit-tag constant unification (codegen 5-9 vs tidepool-heap `LitTag::from_byte`) | S3 C3 |
| P1 | Cache F2 (include-order key) + F3a (content-hash binary fingerprint) | S5, both High |
| P2 | Cache hardening batch: F1 NUL framing, F3b/F4/F5 staleness routes, F6 integrity hash | S5 |
| P2 | `arb_core_expr_depth` cap overshoot; CBOR-roundtrip (Err,Ok) classification | S4 BUG-3/4 |
| P2 | EffectMachine lax Val arity | S2-F1 |
| P3 | Eval deep-recursion ceiling (`eval_at`: the big recursion-crate slice — needs its own plan) | gotcha #5 |
| P3 | Rust-side jump-crossing-Lam rewrite (currently documented precondition) | W2-B2 |
| DECISION | Result-type hint channel in meta.cbor (empty-`[Char]` rendering + richer rendering generally; brushes the types-stripped locked decision) | W8-B1 |

## Recursion-sweep audit (the "recursion crate everywhere" direction, made concrete)

Already iterative/stack-safe: `value_to_heap` (recursion-crate hylo),
`heap_to_value` (worklist + RootScope), `Value::Drop` for Con/ConFun spines,
test comparators (this campaign), `emit/expr.rs` (recursion crate).

Still recursive (audit 2026-06-11):

| Site | Risk | Fix size |
|------|------|----------|
| `EffectMachine::apply_cont` (tidepool-effect/src/machine.rs:160) | deep continuation queues overflow the ORACLE thread (S2-B3, pinned) | small: explicit cont-stack loop |
| `eval::force` (tidepool-eval/src/eval.rs:51) | indirection chains; shallow in practice | trivial: loop |
| `eval::deep_force` (eval.rs:88) | deep structures | moderate: worklist |
| `eval::eval_at` (eval.rs:153, 16 self-calls) | the tree-walker itself; ~50-frame practical ceiling (gotcha #5) | LARGE: own plan; consider recursion crate or explicit machine |
| `Value::Drop` closure-env gap (value.rs:115) | flattener skips Closure/JoinCont env vectors — deep spines inside closure envs still drop recursively | small: extend flatten match |

## Suite inventory (all green at time of writing)

14 new property-test files across 6 crates; every confirmed-then-fixed bug is
an ACTIVE regression test; open findings are `#[ignore = "BUG: ..."]` twins.
Committed proptest-regressions seeds in each suite's directory.
