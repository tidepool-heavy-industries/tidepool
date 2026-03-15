# Tidepool Mutation Testing — Coverage Gaps

Generated: 2026-03-14
Method: Apply subtle logic mutation to production code, run `cargo test --workspace`, revert. Mutations not caught by any test are logged here.

---

## Uncaught Mutations (Coverage Gaps)

### 1. Flipped lambda shadowing check — `tidepool-repr/src/subst.rs:97`

**Mutation:** `if actual_binder == ctx.target` → `if actual_binder != ctx.target`

**What it breaks:** When a lambda binder shadows the substitution target, we should stop substituting (copy the subtree as-is). With this mutation, we substitute *into* the shadowed scope and copy the subtree *when not shadowed* — inverting the logic entirely.

**Why it matters:** Substitution correctness is foundational to all optimizations (beta reduction, inlining). A flipped shadowing check silently corrupts variable bindings in any expression with a lambda that shadows a let-bound variable.

**Gap:** No test exercises substitution into a lambda where the binder *equals* the substitution target. The capture-avoidance tests exist (`test_beta_capture_avoiding`, `test_subst_shadowing`) but they only cover the capture case and the "proceed normally" case — not the "shadowed so stop" case with the condition inverted.

---

### 2. Wrong env in Join body substitution — `tidepool-repr/src/subst.rs:298`

**Mutation:** `subst_at(tree, *body, ctx, env)` → `subst_at(tree, *body, ctx, &join_env)`

**What it breaks:** The Join body is substituted using the Join-parameter-renamed environment (`join_env`) instead of the outer environment (`env`). This means renamed Join parameters leak into the body scope where they don't belong.

**Why it matters:** Substitution through `Join` nodes is subtle — the RHS uses renamed params but the body (the continuation) doesn't. Mixing these environments causes incorrect variable resolution in any program with a Join whose params were renamed during substitution.

**Gap:** `test_subst_join` catches the shadow case, but no test specifically checks that Join body substitution uses the *original* env while the RHS uses the renamed one.

---

### 3. Removed transitive thunk forcing — `tidepool-eval/src/eval.rs:44`

**Mutation:** `ThunkState::Evaluated(v) => force(v, heap)` → `ThunkState::Evaluated(v) => Ok(v.clone())`

**What it breaks:** When an evaluated thunk contains another thunk (a chain: ThunkRef → ThunkRef → value), the recursive `force` call is skipped, returning the intermediate thunk instead of the final value.

**Why it matters:** Thunk chains arise naturally with lazy data structures like infinite lists (`fibs`, `ones`, etc.) and any program using `map` over a lazy list. Without transitive forcing, the evaluator would return `ThunkRef` values where the rest of the code expects concrete values, causing silent corruption.

**Gap:** The test suite has lazy list tests (`fibs`, `xs = 1 : map (+1) xs`) but they apparently don't produce ThunkRef chains deep enough to distinguish `force(v)` from `Ok(v.clone())`. A test that chains two levels of lazy evaluation (a thunk whose result is itself a fresh thunk) would catch this.

---

### 4. Removed LetRec lambda eager evaluation — `tidepool-eval/src/eval.rs:211`

**Mutation:** `if matches!(&expr.nodes[*rhs_idx], CoreFrame::Lam { .. })` → `if false`

**What it breaks:** In `letrec`, lambda RHSes are eagerly evaluated and back-patched into the thunk slot so mutual recursion works (the closure captures the already-bound thunk IDs). Disabling this means lambdas in `letrec` go through the general thunk path instead, breaking knot-tying for mutually recursive functions.

**Why it matters:** Every mutually recursive function (e.g., `even`/`odd`, `isEven`/`isOdd`) depends on this. If lambdas in `letrec` are not eagerly evaluated, the circular references won't be resolved correctly.

**Gap:** The test suite runs mutual recursion (`even`/`odd`) but the thunk path apparently also happens to work in those specific cases, perhaps because those tests don't exercise the distinction between the lambda-eager path and the thunk path in a way that fails.

---

### 5. IntShra right shift swapped with left shift — `tidepool-eval/src/eval.rs:431`

**Mutation:** `a.wrapping_shr(b as u32)` → `a.wrapping_shl(b as u32)`

**What it breaks:** The arithmetic right shift primop (`IntShra`) now performs a left shift, returning wildly incorrect results for any bitwise shift operation.

**Why it matters:** Shift operations are used internally by GHC's generated code for various numeric computations. A silently wrong shift would produce incorrect results without any type error.

**Gap:** No test exercises `IntShra` specifically. The primop is implemented but never tested — the haskell suite tests that exercise bit operations apparently don't use arithmetic right shift.

---

### 6. SubWordCCarry boundary condition — `tidepool-eval/src/eval.rs:1626`

**Mutation:** `if a < b { 1 } else { 0 }` → `if a <= b { 1 } else { 0 }`

**What it breaks:** `subWordC#` reports a borrow (carry) when `a == b` even though `a - b = 0` with no borrow. This is an off-by-one in the carry flag logic for multi-word arithmetic.

**Why it matters:** Multi-precision arithmetic (used internally by GHC's Integer/Natural types) depends on correct carry propagation. A wrong carry at `a == b` would corrupt any multi-word subtraction where a word-level subtraction produces exactly zero.

**Gap:** No test exercises `SubWordCCarry` with equal arguments. The equal-arguments edge case is the one that distinguishes `<` from `<=`.

---

### 7. LetRec DCE all→any — `tidepool-optimize/src/dce.rs:48`

**Mutation:** `.all(|(binder, _)| get_occ(...) == Occ::Dead)` → `.any(|(binder, _)| get_occ(...) == Occ::Dead)`

**What it breaks:** A `LetRec` group should only be dropped if *all* its binders are dead. With `any`, the entire group gets dropped if even one binder is unused — silently deleting live code.

**Why it matters:** This would cause crashes or wrong results for any program with a mutually recursive group where some (but not all) members are referenced. The live members get deleted along with the dead ones.

**Gap:** The DCE tests cover single-binder cases and the "all dead" case, but no test has a `LetRec` with mixed liveness (some binders live, some dead). Such a test would be: `let rec { f = ...; g = ... } in f ()` where `g` is never called — the group should be kept because `f` is live.

---

### 8. Nursery growth threshold — `tidepool-heap/src/arena.rs:147`

**Mutation:** `live_bytes > pre_gc_capacity * 3 / 4` → `live_bytes > pre_gc_capacity * 1 / 2`

**What it breaks:** The nursery grows when live data after GC exceeds 50% of capacity instead of 75%. This causes the nursery to grow more aggressively than designed — higher memory use.

**Why it matters:** While this won't cause correctness failures, it changes the memory profile significantly and could mask GC bugs in programs that rely on the nursery staying small.

**Gap:** No test checks the nursery growth policy. The GC tests only verify that collection happens and data survives, not that the nursery size follows the intended growth heuristic.

---

### 9. GC drops ThunkRef tracing — `tidepool-heap/src/arena.rs:166`

**Mutation:** `Value::ThunkRef(id) => vec![*id]` → `Value::ThunkRef(_id) => vec![]`

**What it breaks:** The GC's reachability analysis no longer follows `ThunkRef` pointers. Any thunk reachable only through another thunk's evaluated value would be collected prematurely, causing use-after-free.

**Why it matters:** This is a silent memory safety bug. Programs with lazy data structures where values are thunks-of-thunks would see arbitrary memory corruption after GC runs.

**Gap:** The GC tests apparently don't create a scenario where a live thunk is reachable *only* through another thunk's value. A test that: (1) creates thunk A evaluating to ThunkRef(B), (2) runs GC, (3) forces B — would catch this.

---

### 10. JIT address range boundary shift — `tidepool-codegen/src/stack_map.rs:80`

**Mutation:** `addr >= *start && addr < *end` → `addr > *start && addr <= *end`

**What it breaks:** The frame walker uses this to identify JIT return addresses. Shifting both boundaries by one means the first address of a function (`addr == *start`) is no longer recognized as JIT code, and the one-past-the-end address (`addr == *end`) is incorrectly treated as JIT code.

**Why it matters:** A frame walker that fails to recognize the entry point of a JIT function would stop the GC walk prematurely, missing live roots. Or it might walk off the end of the function into unmapped memory.

**Gap:** The stack map tests check the general end-to-end flow but don't specifically test the boundary conditions (`contains_address` at exactly `*start` and exactly `*end`). Unit tests for `contains_address` with boundary values would catch this.

---

## Summary

| # | File | Mutation | Result |
|---|------|----------|--------|
| 1 | `tidepool-repr/src/subst.rs:97` | Flipped lambda shadow check `==` → `!=` | **MISSED** |
| 2 | `tidepool-repr/src/subst.rs:99` | Lambda shadow: `copy_with_env` → `subst_at` | Caught (`test_subst_shadowing`) |
| 3 | `tidepool-repr/src/subst.rs:100` | Capture avoidance: `fvs_replacement.contains` → `false` | Caught (`test_beta_capture_avoiding`) |
| 4 | `tidepool-repr/src/subst.rs:280` | Join param shadow: `actual_p == ctx.target` → `false` | Caught (`test_subst_join`) |
| 5 | `tidepool-repr/src/subst.rs:298` | Join body: `env` → `&join_env` | **MISSED** |
| 6 | `tidepool-eval/src/eval.rs:44` | Transitive forcing: `force(v)` → `Ok(v.clone())` | **MISSED** |
| 7 | `tidepool-eval/src/eval.rs:211` | LetRec lambda: eager check → `false` | **MISSED** |
| 8 | `tidepool-eval/src/eval.rs:263` | Case matching: `==` → `!=` | Caught (`interpreter_matches_jit`) |
| 9 | `tidepool-eval/src/eval.rs:398` | IntLt: `<` → `<=` | Caught (haskell suite) |
| 10 | `tidepool-eval/src/eval.rs:431` | IntShra: `wrapping_shr` → `wrapping_shl` | **MISSED** |
| 11 | `tidepool-eval/src/eval.rs:465` | WordQuot: `wrapping_div` → `wrapping_mul` | Caught (`prim_quot_rem_word`) |
| 12 | `tidepool-eval/src/eval.rs:1626` | SubWordCCarry: `<` → `<=` | **MISSED** |
| 13 | `tidepool-optimize/src/occ.rs:34` | Occ accumulation: `entry.add(Once)` → `Once` | Caught (occ + inline tests) |
| 14 | `tidepool-optimize/src/dce.rs:37` | DCE LetNonRec: `==Dead` → `!=Dead` | Caught (dce tests) |
| 15 | `tidepool-optimize/src/dce.rs:48` | LetRec DCE: `.all` → `.any` | **MISSED** |
| 16 | `tidepool-heap/src/arena.rs:50` | Alignment: `+7` → `+8` | Caught (GC/stack-map tests) |
| 17 | `tidepool-heap/src/arena.rs:76` | Nursery limit: `>` → `>=` | Caught (`test_nursery_exhaustion`) |
| 18 | `tidepool-heap/src/arena.rs:147` | Growth threshold: `3/4` → `1/2` | **MISSED** |
| 19 | `tidepool-heap/src/arena.rs:166` | GC ThunkRef tracing: `vec![*id]` → `vec![]` | **MISSED** |
| 20 | `tidepool-codegen/src/stack_map.rs:80` | JIT range: `>=start && <end` → `>start && <=end` | **MISSED** |

**10 of 20 mutations survived = 50% mutation score**
