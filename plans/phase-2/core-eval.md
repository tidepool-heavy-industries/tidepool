# Phase 2: core-eval

**Owner:** Claude TL (depth 1, worktree off main)
**Branch:** `main.core-eval`
**Depends on:** phase-1/core-repr (CoreFrame, MapLayer, supporting types)
**Produces:** Tree-walking evaluator for CoreExpr. Becomes the test oracle for core-optimize and codegen.

core-heap depends on the scaffold from this spec (HeapObject layout, Heap trait). core-optimize depends on the completed evaluator. Scaffold early to unblock dependents.

---

## Wave 1: Scaffold (1 worker, gate)

### scaffold-eval

**Task:** Write the core evaluation types: Value, Env, EvalError, Heap trait, HeapObject layout, ThunkState, Pass trait.

**Read First:**
- `tidepool-plans/decisions.md` (D5, D6, D7 — HeapObject layout decisions)
- `tidepool-plans/anti-patterns.md`
- `core-repr/src/frame.rs` (CoreFrame definition from phase 1)

**Steps:**
1. Create `core-eval/src/value.rs` — Value enum with variants: `Lit(Literal)`, `Con(DataConId, Vec<Value>)`, `Closure(Env, VarId, CoreExpr)`, `ThunkRef(ThunkId)`, `JoinCont(Vec<VarId>, CoreExpr)`, `ContLeaf(Closure)`, `ContNode(Box<Value>, Box<Value>)`. Continuations from freer-simple are type-aligned sequences: `Leaf` wraps a single closure, `Node` composes two continuations as a binary tree (see decisions.md "freer-simple Architecture").
2. Create `core-eval/src/env.rs` — `Env = HashMap<VarId, Value>` (or `im::HashMap` for persistent sharing)
3. Create `core-eval/src/error.rs` — EvalError enum: `UnboundVar(VarId)`, `TypeMismatch { expected, got }`, `NoMatchingAlt`, `InfiniteLoop(ThunkId)`, `UnsupportedPrimOp(PrimOpKind)`, `HeapExhausted`
4. Create `core-eval/src/heap.rs` — Heap trait (`alloc`, `force`, `read`, `write`, `gc_roots`, `trigger_gc`), HeapObject memory layout per decisions.md (raw byte buffers with unsafe accessors — NOT a Rust enum, see decisions.md), ThunkState enum, `ThunkId(u32)` newtype
5. Create `core-eval/src/pass.rs` — `trait Pass { fn run(&self, expr: &mut CoreExpr) -> Changed; }`, `type Changed = bool`
6. Create `core-eval/src/lib.rs` — crate re-exports

**Context:**
Two memory models coexist. The interpreter uses Value/Env (Rust-allocated, GC-invisible). The codegen path uses HeapObject (arena-allocated, GC-traced). The interpreter only touches the arena for thunk allocation (ThunkRef); closures and values live in normal Rust memory. ThunkId is u32 (index), NOT a raw pointer. GC never needs to trace the Rust call stack for the interpreter path.

**Verify:** `cargo test -p core-eval`

**Done:** All types compile. EvalError Display is human-readable. HeapObject repr(C) layout verified with `std::mem::offset_of` (tag at offset 0). JoinCont can be stored in and retrieved from Env.

**Boundary:**
- HeapObject layout is FROZEN after this wave. Both interpreter and codegen consume it.
- ThunkId is u32 index, NOT a raw pointer. Do not use `*mut HeapObject` in the interpreter path.

**Gate:** TL reviews scaffold output before spawning wave 2. Verify type signatures match decisions.md exactly. This scaffold also unblocks core-heap TL.

---

## Wave 2: Strict Eval + Case (2 workers, parallel)

### eval-strict

**Task:** Implement strict evaluation algebra: `CoreFrame<Value>` → `Result<Value, EvalError>`.

**Read First:**
- `core-eval/src/value.rs` (scaffold output from wave 1)
- `core-eval/src/env.rs`
- `core-repr/src/frame.rs` (CoreFrame variants)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-eval/src/eval.rs` with `fn eval(expr: &CoreExpr, env: &Env) -> Result<Value, EvalError>`
2. Handle Lit → `Value::Lit(literal)`
3. Handle Var → env lookup, `EvalError::UnboundVar` if missing
4. Handle Con → `Value::Con(tag, evaluated_fields)`
5. Handle Lam → `Value::Closure(env.clone(), binder, body)`
6. Handle App → match fun on Closure, extend env with `binder=arg`, eval body
7. Handle LetNonRec → eval binding, extend env, eval body
8. Handle LetRec → allocate all bindings, extend env, eval body
9. Handle PrimOp → dispatch to native Rust operations for arithmetic/comparison

**Verify:** `cargo test -p core-eval -- eval`

**Done:** All strict CoreFrame variants handled. No `todo!()` arms. Tests pass.

**Tests:**
- `(λx.x) 42 == 42`
- `let x = 1 in x + x == 2`
- Unbound variable → `EvalError::UnboundVar`
- PrimOp `+#` on two ints → correct result

**Boundary:**
- Do NOT manually force strict Con fields. Post-simplifier Core already has explicit case expressions for strictness (GHC's worker/wrapper transform). Manual forcing would double-evaluate.
- Do NOT implement Case — that's a separate worker.

---

### eval-case

**Task:** Implement case evaluation: force scrutinee to WHNF, match against alts.

**Read First:**
- `core-eval/src/eval.rs` (eval-strict output)
- `core-eval/src/value.rs`
- `core-repr/src/frame.rs` (Alt type)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Add case dispatch to `eval.rs`: evaluate scrutinee to WHNF, bind the **case binder** to the result in env, then match alts
2. Handle constructor alts (`DataAlt`): match `Value::Con(tag, fields)` against alt DataConId, bind fields to alt binders by position
3. Handle literal alts (`LitAlt`): match `Value::Lit` against literal pattern
4. Handle default alt (`Default`): used when no constructor/literal alt matches. The case binder is still bound.
5. Missing alt (no default, no matching pattern) → `EvalError::NoMatchingAlt` (not panic)
6. Well-formed Core has at most one alt per constructor and at most one Default. Do NOT assume overlapping alts — if duplicates appear, the serializer has a bug.

**Verify:** `cargo test -p core-eval -- case`

**Done:** Case dispatch works for constructor, literal, and default alts. Nested case-of-case works.

**Tests:**
- `case Just(42) of w { Just x → x; Nothing → 0 } == 42` (case binder `w` bound but unused)
- `case Just(42) of w { Just x → w } == Just(42)` (case binder `w` used — returns whole scrutinee)
- `case 3 of { 1 → a; 2 → b; _ → c } == c`
- `case Nothing of { Just x → x } → EvalError::NoMatchingAlt`
- Nested: `case (case x of ..) of ..`
- Constructor with multiple fields: all bound correctly by position

**Boundary:**
- Do NOT modify the eval function signature. Add case handling alongside existing variants.
- Bind ALL fields by position, not just the first.

---

**After wave 2:** TL wires eval-strict + eval-case (if needed — workers may have already integrated since they share the same file). `cargo test -p core-eval`. Commit.

---

## Wave 3: Lazy Eval (2 workers, sequential or parallel)

### thunks

**Task:** Add lazy evaluation via thunk allocation and forcing. Modify Let to allocate thunks, Case to force scrutinee.

**Read First:**
- `core-eval/src/eval.rs` (waves 1-2 output)
- `core-eval/src/heap.rs` (Heap trait, ThunkState)
- `tidepool-plans/decisions.md` (D5, D7 — thunk sizing, indirection)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Modify LetNonRec: if RHS is a Lam, eval to `Value::Closure` immediately (lambdas are values, not computations). Otherwise, `Heap::alloc(env, rhs_expr)` → ThunkId, store as `Value::ThunkRef(id)`.
2. Modify Case scrutinee: `Heap::force(thunk_id)` → Value before matching
3. Implement force protocol: Unevaluated → overwrite to BlackHole → eval → overwrite to Evaluated
4. BlackHole on force → `EvalError::InfiniteLoop`
5. Evaluated on force → return cached value
6. Implement LetRec knot-tying:
   a. Distinguish lambda-RHSes from non-lambda-RHSes. Lambda-RHSes are closures (values); non-lambda-RHSes are thunks (suspended computations). In post-simplifier Core, LetRec RHSes are almost always lambdas (recursive functions).
   b. For thunk-RHSes: allocate at Unevaluated size, tag = BlackHole initially
   c. Build new env mapping each binder to its ThunkId (thunks) or Closure (lambdas — the closure captures the new env, enabling mutual recursion)
   d. Back-patch thunks: change tag from BlackHole to Unevaluated, fill in env pointer
   e. Size does NOT change — only tag and env field are mutated

**Context:**
The LetRec knot-tying protocol is the only place mutation of thunk internals is allowed outside the force→Evaluated transition. Implement the EXACT state machine from the Heap trait spec. No improvisation on thunk lifecycle.

**Verify:** `cargo test -p core-eval -- thunk`

**Done:** Lazy evaluation correct. Force-twice returns cached value. Mutual recursion works. BlackHole detection works.

**Tests:**
- Lazy: unused divergent let doesn't crash (`let x = error in 42 == 42`)
- Caching: force-twice returns same value, eval runs only once
- `let x = x in x` → `EvalError::InfiniteLoop`
- LetRec mutual recursion: `let { f = \x -> g x; g = \x -> f x } in f 0` → InfiniteLoop

**Boundary:**
- Do NOT improvise on thunk lifecycle. Follow the exact state machine: Unevaluated → BlackHole → Evaluated.
- LetRec back-patching is the ONLY mutation allowed outside force. No other code should write thunk internals.

---

### join-points

**Task:** Implement Join/Jump as CoreFrame variants with tail-call semantics.

**Read First:**
- `core-eval/src/eval.rs` (current evaluator)
- `core-eval/src/value.rs` (JoinCont variant)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Handle Join: bind label → `Value::JoinCont(params, rhs)` in env, then eval body
2. Handle Jump: look up JoinCont by label in env, bind params to args, eval rhs
3. Direct transfer: no stack push, no thunk allocation
4. Nested join points: inner shadows outer label in env
5. Jump to nonexistent label → `EvalError::UnboundVar` (not panic)

**Context:**
JoinCont is a Value variant — never heap-allocated, never forced, lives only in Env. Reference: Maurer et al. "Compiling without Continuations" ICFP 2017.

**Verify:** `cargo test -p core-eval -- join`

**Done:** Join/Jump works. Nested joins shadow correctly. Bad jumps produce errors not panics.

**Tests:**
- Simple join/jump evaluates correctly
- Join with 3 params — all bound correctly on jump
- Nested joins — inner shadows outer label
- Jump to nonexistent label → EvalError (not panic)
- Join reduces allocation vs equivalent let+call

**Boundary:**
- JoinCont is NOT a thunk. Do not heap-allocate it. It lives in Env only.
- Do NOT push a stack frame for Jump. It's a direct transfer.

---

**After wave 3:** TL integrates lazy eval. Adds `EvalStrategy` enum (Strict/Lazy) to select mode. `cargo test -p core-eval`. Commit. File PR when all waves complete.

---

## Domain Anti-Patterns

In addition to the base anti-patterns file, these apply to all core-eval workers:

- Do NOT manually force strict Con fields. GHC's core2core pass already inserted explicit case expressions.
- `JoinCont` is NOT a thunk. Never heap-allocate, never force.
- ThunkId is a u32 index. The interpreter NEVER holds raw `*mut HeapObject`.
- Implement the EXACT thunk state machine. No creative alternatives.
