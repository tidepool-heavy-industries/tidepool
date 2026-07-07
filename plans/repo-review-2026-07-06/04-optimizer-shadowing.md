# 04 — Optimizer shadowing soundness + differential-net blindness

Two EXECUTED-REPRO miscompiles in PartialEval, the same latent class on the
production path in normalize.rs, and the structural reason no test ever caught
them: the proptest generator cannot emit shadowed binders. Fix as ONE
shadowing-hardening commit + a generator mode, so the class stays covered.

Mitigating context (why this isn't tier 1): the optimizer is NOT on the
production JIT compile path (no `tidepool-runtime/src` usage of the passes),
and GHC-produced Core has unique VarIds. But shadowing is legal Core, the
pipeline's own `subst` can create duplicate binder ids from DAG-sharing
inputs, and normalize.rs IS on the production path.

## ANTI-PATTERNS

- Do NOT fix by renaming/freshening binders in the passes — the bug is missing
  `Unknown` env entries, a 2-line fix each. Beta/Inline/DCE/CaseReduce were
  verified shadow-aware; don't "harden" them speculatively.
- Do NOT add the generator shadowing mode without also fixing the passes first
  — it will (correctly) turn the differential suites red.

## READ FIRST

- `tidepool-optimize/src/partial.rs` — compare the `Lam`/`Join` arms against
  `LetNonRec`/`LetRec`/`Case`, which all correctly insert binders
- `tidepool-testing/src/gen/strategy.rs:129-160` — the shared fresh-var
  counter (`Rc<Cell<u64>>` across all contexts) that makes shadowing
  ungenerable

---

> **STATUS: DONE** — F1/F2/F3 in `81e3b91f`, F5/F6 in `fecd34b7`, F4 in the
> commit carrying this note. See the per-finding STATUS blocks below.
> (Worker session ended before committing F4 + these annotations; root
> reviewed the diff, re-ran the suites, and committed.)

## F1 (HIGH, EXECUTED REPRO): PartialEval `Lam` arm ignores binder shadowing

**Where:** `tidepool-optimize/src/partial.rs:253-264` — evaluates the body
with the incoming `PartialEnv` without inserting `binder → Unknown`.

**Repro (executed during review):** `let x = 1 in ((\x -> x) 99)` evaluates to
`99` before the pass and `1` after — PartialEval rewrites the identity lambda
to `\x -> 1`.

**Fix:** `new_env.insert(*binder, PartialValue::Unknown)` before evaluating
the body.

> **STATUS: DONE** (commit `81e3b91f`). Fixed exactly as prescribed:
> `new_env.insert(*binder, PartialValue::Unknown)` before descending into the
> Lam body. Regression test `test_partial_lam_binder_shadowing` (in
> `tidepool-optimize/src/partial.rs`'s `#[cfg(test)] mod tests`) uses the
> plan's exact repro. Observed RED before the fix: `after` evaluated to
> `LitInt(1)` (both `before` and `after` printed via a temporary revert),
> confirming the miscompile; GREEN after (`before == after == 99`).

## F2 (HIGH, EXECUTED REPRO): same hole for `Join` params

**Where:** `partial.rs:265-284` — `rhs` evaluated with the incoming env,
params never marked Unknown.

**Repro:** `let x = 1 in join j(x) = x in jump j(42)` → `42` before, `1`
after.

**Fix:** insert each param as `Unknown` in the env used for `rhs`.

> **STATUS: DONE** (commit `81e3b91f`). Fixed exactly as prescribed: each
> param inserted as `PartialValue::Unknown` into the env used for `rhs`
> (params scope over `rhs` only, not `body` — mirrors how the verified-clean
> passes treat Join). Regression test `test_partial_join_param_shadowing`
> uses the plan's exact repro. Observed RED before the fix: `after` evaluated
> to `LitInt(1)`; GREEN after (`before == after == 42`).

### Embedded repro program

Standalone crate; recreate anywhere with path-deps on `tidepool-optimize`,
`tidepool-repr`, `tidepool-eval` — or skip the crate and write it directly as
two `#[test]`s in tidepool-optimize (preferred; assert before == after ==
99/42). `cargo run` prints before/after (LAM 99→1, JOIN 42→1 while unfixed).

```rust
use tidepool_optimize::{partial::PartialEval, Pass};
use tidepool_repr::{CoreExpr, CoreFrame, JoinId, Literal, VarId};

fn main() {
    // Repro 1: let x = 1 in (\x -> x) 99  — must stay 99.
    let x = VarId(7);
    let nodes = vec![
        CoreFrame::Lit(Literal::LitInt(1)),    // 0: outer rhs
        CoreFrame::Var(x),                     // 1: lambda body (inner x)
        CoreFrame::Lam { binder: x, body: 1 }, // 2: \x -> x
        CoreFrame::Lit(Literal::LitInt(99)),   // 3
        CoreFrame::App { fun: 2, arg: 3 },     // 4
        CoreFrame::LetNonRec { binder: x, rhs: 0, body: 4 }, // 5
    ];
    let mut expr = CoreExpr { nodes };
    let mut heap = tidepool_eval::VecHeap::new();
    let before = tidepool_eval::eval(&expr, &tidepool_eval::Env::new(), &mut heap).unwrap();
    PartialEval.run(&mut expr);
    let mut heap2 = tidepool_eval::VecHeap::new();
    let after = tidepool_eval::eval(&expr, &tidepool_eval::Env::new(), &mut heap2).unwrap();
    println!("LAM  before: {:?}\nLAM  after:  {:?}", before, after);

    // Repro 2: let x = 1 in join j(x) = x in jump j(42)  — must stay 42.
    let nodes = vec![
        CoreFrame::Lit(Literal::LitInt(1)),
        CoreFrame::Var(x),
        CoreFrame::Lit(Literal::LitInt(42)),
        CoreFrame::Jump { label: JoinId(1), args: vec![2] },
        CoreFrame::Join { label: JoinId(1), params: vec![x], rhs: 1, body: 3 },
        CoreFrame::LetNonRec { binder: x, rhs: 0, body: 4 },
    ];
    let mut expr = CoreExpr { nodes };
    let mut heap = tidepool_eval::VecHeap::new();
    let before = tidepool_eval::eval(&expr, &tidepool_eval::Env::new(), &mut heap).unwrap();
    PartialEval.run(&mut expr);
    let mut heap2 = tidepool_eval::VecHeap::new();
    let after = tidepool_eval::eval(&expr, &tidepool_eval::Env::new(), &mut heap2).unwrap();
    println!("JOIN before: {:?}\nJOIN after:  {:?}", before, after);
}
```

## F3 (MEDIUM, PRODUCTION PATH): `normalize.rs` global `var_map` is last-wins with no scoping

**Where:** `tidepool-repr/src/normalize.rs:67-79`. Same duplicate-VarId
shadowing class as F1/F2 but ON the JIT compile path; currently masked by
GHC-unique ids. Fold into the same hardening commit: scope the map (or assert
uniqueness loudly at entry so a violation can't silently miscompile).

> **STATUS: DONE** (commit `81e3b91f`). The flat, pre-pass `var_map` was
> replaced with `collect_scoped_bindings`: a single top-down (root-first)
> walk that threads a properly-scoped env (clone-and-insert on entering a
> `Let`'s body, clone-and-remove on entering a `Lam`/`Case`-alt/`Join`'s
> scope), mirroring how `PartialEval`/`subst` thread theirs — NOT a loud
> assert, since duplicate VarIds are legal Core here. Two new regression
> tests: `prim_args_not_unboxed_through_lam_shadowed_var` (a Lam binder
> shadowing an outer known-boxed let — must stay opaque, no rule fires) and
> `effect_tag_var_map_respects_duplicate_binder_shadowing` (two nested
> `LetNonRec`s reusing the same VarId — the tag must resolve through the
> INNER rhs, not whichever binding a flat scan visits last). All 18 existing
> + new `normalize` unit/proptest tests green.

## F4 (STRUCTURAL): the proptest generator cannot emit shadowed binders

**Where:** `tidepool-testing/src/gen/strategy.rs:129-160` — the fresh-var
counter is a shared `Rc<Cell<u64>>` across all generation contexts, so no
binder id is ever reused. The ENTIRE differential net is structurally blind to
the shadowing bug class — that is why F1/F2 survived.

**Fix:** add a generator mode/weight that deliberately reuses an in-scope
binder id for a fraction of Lam/Let/Case/Join binders, and run the pass-
preservation + JIT-vs-eval suites with it enabled. NOTE ordering: land F1-F3
first. Also see plan 08 F1 — `check_pass_preserves_eval` must deep-force
before comparing, or the new mode still can't see the bug under a lazy field.

> **STATUS: DONE**. `arb_core_expr_shadowing(depth, shadow_pct)` added to
> `tidepool-testing` — a `shadow_weight` on the generation `Context` makes
> each Lam/Let/Case/Join binder reuse an in-scope same-type `VarId` with
> proptest-controlled probability (`prop::bool::weighted`, so cases shrink
> and reproduce). Weight 0 (the default) reproduces the historical
> always-fresh generators exactly; only the new entry point sets it.
> `tidepool-optimize/tests/proptest_shadowing.rs` runs pass-preservation
> (PartialEval alone + the full `optimize` pipeline) and JIT-vs-eval
> differential at 800 cases each with 60% shadow weight — 3/3 green.
> The JIT lane `prop_assume!`s out `check_toplevel_varids` rejections:
> that guard defends the #313 top-level-collision class, a precondition
> real extractor output satisfies but the synthetic generator (by design)
> can violate with legitimate local shadowing. Landed AFTER F1-F3 as
> ordered; deep-force compare (plan 08 F1) confirmed present in the
> harness this suite routes through.

## F5 (MEDIUM): `Word64Shrl` missed the wrapping-shift fix its siblings got

**Where:** `tidepool-eval/src/eval.rs:2175-2178` — raw `>>`; the EVAL-1/2/3
siblings (`Int64Negate`/`Int64Shra`/`Word64Shl`) were fixed to `wrapping_*`.
`uncheckedShiftRL64#` with shift ≥ 64 panics the oracle in debug builds while
the JIT (Cranelift `ushr`, mod-64 mask) returns a value.

**Fix:** `a.wrapping_shr(b as u32)`; add the op to `prop_word_shift` (it
currently never exercises `Word64Shrl`).

> **STATUS: DONE** (commit `fecd34b7`). Fixed exactly as prescribed;
> `prop_word_shift` now covers `Word64Shrl`, and
> `evalbug4_word64_shrl_shift_64` pins the shift-≥64 repro alongside the
> existing evalbug1-3 fixed-bug repros.

## F6 (COVERAGE ROT): stale panic-tolerant routing for the ALREADY-FIXED EVAL-1/2/3 ops

**Where:** `tidepool-codegen/tests/proptest_primops_differential.rs:497-499,
513-515, 561-563` — the three fixed ops still route through
`run_oracle_eval_may_panic`, which tolerates an eval panic, so a REGRESSION in
any of them keeps the suite green.

**Fix:** switch to strict `run_oracle`; after F5 lands, delete
`run_oracle_eval_may_panic` entirely (it would then guard nothing).

> **STATUS: DONE** (commit `fecd34b7`). All three EVAL-1/2/3 call sites
> routed through strict `run_oracle`; `run_oracle_eval_may_panic`, the
> `N_EVAL_PANIC` counter, and its reach-floor report line deleted.

## Opportunities

- **Inline/Beta lack an OnceL gate:** a Once binder inside a `Lam` body gets
  inlined, re-evaluating the RHS per call (work duplication, not soundness —
  GHC distinguishes `OnceL`). Occ counting counts nodes rather than DAG edges
  (acknowledged in beta.rs's comment). Worth fixing if the optimizer is ever
  wired into the production compile path.
- `insert_checked`'s idempotent path still last-wins on tag/arity disagreement
  under an agreeing qualified name — cheap hardening (cross-ref plan 05).
- A `child < parent` debug_assert in `extract_subtree`/`replace_subtree`/
  `free_vars` complements plan 05 F1's wire-level cycle check for internal
  constructors.

## Verified clean — do NOT re-audit

Beta/Inline/DCE/CaseReduce are sound under shadowing — `subst` is genuinely
shadow-aware and capture-avoiding, including Join params scoping over rhs only
(pinned by tests). Occ-analysis conflation only inflates counts
(conservative). `free_vars` per-index memo sound. PartialEval's wrapping
constant-folds match eval's wrapping arms; comparison primops return unboxed
`LitInt 0/1` consistently in eval, JIT, and PartialEval's `int_cmp`. All other
eval shift arms are `wrapping_*`. `quotInt# minBound (-1)` divergence is known
and deliberately excluded.

## DONE CRITERIA

- [x] F1/F2 fixed; embedded repros added as regression tests (assert 99/42
      preserved)
- [x] F3 scoped or loudly asserted (scoped — `collect_scoped_bindings`;
      loud assert rejected as duplicate VarIds are legal Core here)
- [x] F5 fixed + `prop_word_shift` covers Shrl; F6 strict routing, tolerance
      machinery deleted
- [x] F4 generator shadowing mode landed AFTER F1-F3, suites green with it on
- [x] `cargo nextest run -p tidepool-optimize -p tidepool-repr` +
      differential lanes green
