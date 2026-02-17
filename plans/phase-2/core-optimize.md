# Phase 2: core-optimize

**Owner:** Claude TL (depth 1, worktree off main)
**Branch:** `main.core-optimize`
**Depends on:** core-eval (working evaluator as test oracle)
**Produces:** Optimization pass pipeline + first-order partial evaluator. Universal property: `eval(pass(e)) == eval(e)`.

---

## Wave 1: Scaffold + Basic Passes (3-4 workers, parallel)

### scaffold-opt

**Task:** Pass trait and fixed-point pipeline infrastructure.

**Read First:**
- `core-eval/src/pass.rs` (Pass trait definition from core-eval scaffold)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/pipeline.rs` — `Vec<Box<dyn Pass>>`, fixed-point iteration: `loop { changed = false; for pass in &passes { changed |= pass.run(&mut expr); } if !changed { break; } }`
2. Stats: per-pass invocation count, total iterations

**Verify:** `cargo test -p core-optimize -- pipeline`

**Done:** Empty pipeline returns unchanged. Single pass runs once. Fixed-point terminates.

---

### occ-analysis

**Task:** Occurrence analysis collapse: `CoreFrame<OccMap>` → `OccMap`. Per-variable: Dead | Once | Many.

**Read First:**
- `core-repr/src/frame.rs` (CoreFrame variants, binding forms)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/occ.rs`
2. Count through LetNonRec (body only, not binding-site), LetRec (sibling RHSes DO count), Lam (exclude binding occurrence), Case (scrutinee + alt bodies; case binder is a binding site — exclude from free count, track its usage in alt bodies), Join (count in body, not label site)

**Verify:** `cargo test -p core-optimize -- occ`

**Done:** Correct occurrence counts for all binding forms.

**Tests:**
- `let x = 1 in 2` → x Dead
- `let x = 1 in x` → x Once
- `let x = 1 in x + x` → x Many
- `λx. x` → x Once (binding occ excluded)
- `letrec { f = g; g = f }` → both Once in each other
- `case x of w { Just y → y }` → x Once, w Dead, y Once
- `case x of w { Just y → w }` → x Once, w Once, y Dead

---

### beta-reduce

**Task:** Detect and reduce manifest beta redexes: `(λx.e) v` → `e[x:=v]`.

**Read First:**
- `core-repr/src/subst.rs` (capture-avoiding substitution)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/beta.rs`
2. Detect `App(Lam(x, body), arg)` pattern → substitute
3. Capture-avoiding substitution (use subst from core-repr)
4. Do NOT reduce under lambdas or inside thunks — only manifest redexes

**Verify:** `cargo test -p core-optimize -- beta`

**Done:** Beta reduction correct. Property: `eval(beta(e)) == eval(e)`.

**Tests:**
- `(λx.x) 42 → 42`
- `(λx.λy.x) 1 2 → 1` (curried)
- Capture-avoidance: `(λx.λy.x) y` doesn't capture y
- Non-redex `(λx.e)` left alone (no arg)

**Boundary:**
- Capture-avoiding substitution. Always.
- Only manifest redexes. No reduction under lambdas.

---

### case-reduce

**Task:** Case-of-known-con and case-of-known-lit reduction.

**Read First:**
- `core-repr/src/frame.rs` (Case, Alt)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/case_reduce.rs`
2. Case-of-known-con: scrutinee is Con → select matching alt, bind ALL fields by position, substitute case binder with the scrutinee Con value
3. Case-of-known-lit: scrutinee is Lit → select matching alt or default, substitute case binder with the literal
4. Case-of-unknown: leave untouched

**Verify:** `cargo test -p core-optimize -- case_reduce`

**Done:** Known cases reduced. Unknown cases untouched. Property: `eval(case_reduce(e)) == eval(e)`.

**Tests:**
- `case Just(42) of { Just x → x } → 42`
- `case (,) 1 2 of { (,) a b → a + b } → 3`
- `case 3 of { 1 → a; _ → b } → b`
- `case f(x) of { ... }` → untouched

**Boundary:**
- Bind ALL fields by position. Not just the first.
- Do NOT reduce when scrutinee is unknown.

---

**After wave 1:** TL wires passes into pipeline (beta → case-reduce). `cargo test`. Commit.

---

## Wave 2: Inline + DCE (2-3 workers)

### inline-coalg

**Task:** Inlining coalgebra: thread environment down, extend at single-use Let sites.

**Read First:**
- `core-optimize/src/occ.rs` (occurrence analysis)
- `core-repr/src/frame.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/inline/coalg.rs`
2. `(CoreExpr, InlineEnv) → CoreFrame<(CoreExpr, InlineEnv)>`
3. Extend env at single-use LetNonRec sites
4. Do NOT extend at LetRec bindings (recursive Once still means "used in own definition")
5. Do NOT extend at multi-use let

**Verify:** `cargo test -p core-optimize -- inline_coalg`

**Done:** Env extended correctly at single-use sites only.

**Boundary:**
- Do NOT inline recursive bindings. Even if Once.
- Do NOT inline join points via the normal inlining path. Join points have a tail-position invariant (they may only be invoked in tail context). Inlining a join point into a non-tail position breaks this invariant and produces invalid Core. Join point inlining requires a dedicated pass that verifies tail position.

---

### inline-alg

**Task:** Inlining algebra: replace inlined vars, drop dead lets.

**Read First:**
- `core-repr/src/subst.rs` (capture-avoiding substitution)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/inline/alg.rs`
2. `CoreFrame<CoreExpr> → CoreExpr`
3. Replace inlined vars from env
4. Drop dead lets (binding was inlined, no remaining uses)
5. Capture-avoiding substitution

**Verify:** `cargo test -p core-optimize -- inline_alg`

**Done:** Single-use lets eliminated. Multi-use preserved. No capture.

---

### dce

**Task:** Dead code elimination: occ-analysis → drop zero-use lets.

**Read First:**
- `core-optimize/src/occ.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/dce.rs`
2. Run occ-analysis, then drop LetNonRec where binder is Dead
3. LetRec groups: only drop entire group if ALL bindings Dead. If any is live, keep all.

**Verify:** `cargo test -p core-optimize -- dce`

**Done:** Dead lets dropped. LetRec atomicity preserved. Property: `eval(dce(e)) == eval(e)`.

**Tests:**
- Dead LetNonRec dropped
- Live LetNonRec preserved
- LetRec with one dead + one live → group preserved
- LetRec all dead → group dropped

**Boundary:**
- LetRec groups are ATOMIC for DCE. Drop all or none.

---

**After wave 2:** TL composes inline (coalg + alg → `expand_and_collapse`), wires all passes into full pipeline: beta → inline → case-reduce → dce. Fixed-point iteration. `cargo test`. Commit.

---

## Wave 3: Partial Evaluation (4 workers across 2 hylos)

First-order only. Known values = literals, constructors. Unknown = `${vars}` and anything bound to Unknown. Partially-known structures degrade to Unknown in v1.

### scaffold-partial

**Task:** Write PartialValue, PartialEnv, residual representation.

**Read First:**
- `core-eval/src/value.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/partial/types.rs`
2. `PartialValue = Known(Value) | Unknown(VarId)`
3. `PartialEnv = HashMap<VarId, PartialValue>`
4. Residual = CoreExpr (Unknown vars left as Var nodes)

**Verify:** `cargo test -p core-optimize -- partial_types`

---

### subst-coalg

**Task:** Partial substitution coalgebra: thread env, extend at Known lets.

**Read First:**
- `core-optimize/src/partial/types.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/partial/subst_coalg.rs`
2. `(CoreExpr, PartialEnv) → CoreFrame<(CoreExpr, PartialEnv)>`
3. Extend env at Let with Known values (from `${var}` interpolation)
4. Unknown lets: do NOT extend env

**Verify:** `cargo test -p core-optimize -- subst_coalg`

---

### subst-alg

**Task:** Partial substitution algebra: replace Known vars, preserve Unknown.

**Read First:**
- `core-optimize/src/partial/types.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/partial/subst_alg.rs`
2. `CoreFrame<CoreExpr> → CoreExpr`
3. Known var refs → replace with value
4. Unknown var refs → leave as Var node
5. No substitution under shadowing lambdas

**Verify:** `cargo test -p core-optimize -- subst_alg`

---

### reduce-coalg + reduce-alg

**Task:** Partial reduction: Known → reduce (beta, case, con). Unknown → residual.

**Read First:**
- `core-optimize/src/partial/types.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-optimize/src/partial/reduce_coalg.rs` — `(CoreExpr, Env) → CoreFrame<(CoreExpr, Env)>`
2. Create `core-optimize/src/partial/reduce_alg.rs` — `CoreFrame<PartialValue> → PartialValue`
3. Known → reduce. Unknown or higher-order → rebuild as residual.
4. Do NOT reduce App where function is Unknown, Case where scrutinee is Unknown

**Verify:** `cargo test -p core-optimize -- reduce`

**Tests:**
- All-Known → agrees with full eval
- All-Unknown → residual equals original
- Mixed: `case Known(Just 42) of { Just x → Unknown(f) x }` → residual is `Unknown(f) 42`

**Boundary:**
- First-order ONLY. Do not specialize unknown functions.

---

**After wave 3:** TL composes HYLO 1 (subst) → HYLO 2 (reduce) into `partial_eval`. Properties verified via proptest. `cargo test`. Commit. File PR.

---

## Domain Anti-Patterns

In addition to the base anti-patterns file:

- Every pass: `eval(pass(e)) == eval(e)`. Include this proptest in every worker.
- Do NOT inline recursive bindings, even if Once.
- Do NOT inline join points via normal inlining. Join points have a tail-position invariant.
- Case reduction must substitute the case binder, not just bind alt fields.
- Capture-avoiding substitution everywhere.
- LetRec groups atomic for DCE.
- Partial eval is FIRST-ORDER ONLY. No unknown function specialization.
