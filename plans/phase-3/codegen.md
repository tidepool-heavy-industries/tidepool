# Phase 3: codegen

**Owner:** Claude TL (depth 2, worktree off `main.core-eval`)
**Branch:** `main.core-eval.codegen`
**Depends on:** core-eval (evaluator as test oracle), core-heap (arena + GC), core-optimize (passes)
**Produces:** Cranelift backend. CoreExpr → optimized IR → native fn → EffectMachine. Universal property: `compiled(e) == interpreted(e)`.

---

## Execution Model

```
Free (Union r) a = Pure a | Free (Union r (Free ..))
```

This is `Fix` applied to the effect functor. Compiling it to a state machine is an **anamorphism**: Cranelift-compiled code unfolds evaluation into steps, yielding at `Send` constructors. The Rust driver consuming steps is a **catamorphism**: folding the step sequence into real effects + final result. The `step()`/`resume()` loop is a **hylomorphism** across the language boundary.

API mirrors Iterator: `Iterator::next() -> Option<Item>` / `EffectMachine::step() -> Yield<Request, Value>`. Same recursion scheme, different carrier.

**No manual continuation capture.** Evaluating `Free (Union r) a` to WHNF naturally returns `Pure val` or `Send req cont`. The continuation `cont` is already a heap-allocated closure (GHC's `>>=` put it there). `resume(result)` = `eval(App(cont, result))`. Loop. Benefits: clean GC (collect between steps only), Rust driver has full control between steps.

---

## Cranelift Reference

### Calling Convention
Tail calling convention: constant stack space, deep recursion safe. Tail call arg setup does NOT clobber stack-mapped slots (Cranelift segregates outgoing tail-call arg space from local spill slots).

### VMContext
Implicit first arg to all compiled functions:
```rust
#[repr(C, align(16))]
pub struct VMContext {
    alloc_ptr: *mut u8,
    alloc_limit: *const u8,
    gc_trigger: extern "C" fn(*mut VMContext),
}
```

### Alloc Fast-Path (inlined in Cranelift IR)
```
load alloc_ptr → add size → cmp alloc_limit →
  if exceeded: cold block calls gc_trigger
  else: store new alloc_ptr, init at old ptr
```

### Return Convention
Single `*mut HeapObject` as I64, via `append_block_params_for_function_returns`.

### Dispatch
`br_table` on HeapObject u8 tag (matches interpreter's Rust `match` on same byte).

### GC Stack Maps
- `declare_value_needs_stack_map` per heap-ptr SSA value
- Cranelift auto-spills before safepoints (any call while a declared value is live)
- Cranelift auto-reloads forwarded ptrs after call returns
- **CRITICAL:** Block params are NEW SSA values. When a GC ref crosses a block boundary (`brif`, `jump`), the destination block param MUST be re-declared with `declare_value_needs_stack_map`. Failure = silent heap corruption (root missed during GC trace).
- Join points = parameterized Cranelift blocks. Block params for heap ptrs MUST be re-declared.

### Stack Map Registry
`JITModule` does NOT expose stack maps. Must use `Context::compile(isa)` directly to get `CompiledCode`. `CompiledCode` contains `MachBufferFinalized` with `user_stack_maps: SmallVec<(CodeOffset, u32, UserStackMap)>`. JITModule can still be used for memory allocation and linking, but stack map metadata must be intercepted from `Context::compile` output BEFORE passing the code to JITModule.

Global/thread-local `BTreeMap<usize, StackMapInfo>`:
- Key: absolute **return address** (`function_base_ptr + CodeOffset + call_instruction_size`). Cranelift's `CodeOffset` in stack maps refers to the call site; the PC captured during frame walking is the *return address* (instruction after the call). Account for this offset or the lookup will miss.
- Value: `StackMapInfo { frame_size: u32, offsets: Vec<u32> }`
- **Offset semantics VERIFIED** (see research/02): Offsets are **SP-relative, positive**. The math is `root_addr = SP + offset` at the safepoint. The tuple from `user_stack_maps()` is `(CodeOffset, span, UserStackMap)` where `span` = frame size, and `UserStackMap::entries()` yields `(Type, u32)` with u32 = SP offset. Progressive liveness: each safepoint's map only lists roots live at that point.

### Frame Walker (runtime, in gc_trigger)
`gc_trigger` reads its own RBP, steps up ONE frame to reach the JIT caller:
- `caller_rbp = *(rbp)`, `caller_pc = *(rbp+8)`
- Look up `caller_pc` in StackMapRegistry → StackMapInfo
- Compute frame bottom: `rsp = rbp - frame_size`
- For each offset: `root_addr = rsp + offset`, read `*mut HeapObject`, after GC write forwarding addr back
- Advance: `next_pc = *(rbp+8)`, `next_rbp = *(rbp)`
- **Termination: stop when PC is outside JIT memory region** (not when `rbp == 0` — the initial RBP on x86-64 Linux is not guaranteed zero)
- Skip frames without stack map entries (Rust host fns between JIT frames)

Pin to Cranelift commit `32f7835b4f` or compatible release. Settings: `preserve_frame_pointers = true` (REQUIRED for GC frame walking). Rust profile: `force-frame-pointers = true` for `gc_trigger`.

---

## Wave 1: Scaffold (1 worker, gate)

### scaffold-codegen

**Task:** Cranelift setup, VMContext struct, compilation pipeline, stack map registry, function signatures.

**Read First:**
- `core-eval/src/heap.rs` (Heap trait, HeapObject layout)
- `core-heap/src/arena.rs` (ArenaHeap for alloc fast-path)
- `tidepool-plans/decisions.md` (D5, D6 — HeapObject, closure code pointer)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Add `cranelift-codegen`, `cranelift-frontend`, `cranelift-jit` deps to `codegen/Cargo.toml` — pinned version
2. Create `codegen/src/context.rs` — VMContext struct per Cranelift Reference above, `#[repr(C, align(16))]`
3. Create `codegen/src/pipeline.rs` — compilation pipeline:
   - `Context::compile(isa)` → extract stack maps from `CompiledCode` → get code into executable memory
   - **Integration path VERIFIED** (see research/02): Double-compile strategy. Call `ctx.compile(isa)` first to extract stack maps from `CompiledCode.buffer.user_stack_maps()`, then call `module.define_function(func_id, &mut ctx)` which recompiles from the IR. The IR survives both compilations. Cost: 2x compilation per function (acceptable for JIT).
   - Function signature template: tail calling convention, vmctx: i64 first param, return: i64
4. Create `codegen/src/stack_map.rs` — `StackMapRegistry`:
   - `BTreeMap<usize, StackMapInfo>` (key: absolute PC, value: frame_size + offsets)
   - Populated from `CompiledCode.user_stack_maps` after compilation
5. Create `codegen/src/host_fns.rs` — extern host fn declarations: `gc_trigger`, `heap_alloc`, `heap_force`
6. Create `codegen/src/alloc.rs` — alloc fast-path IR snippet (inline bump pointer check)
7. Cranelift settings: `preserve_frame_pointers = true`
8. Create `codegen/src/lib.rs` — crate re-exports

**Verify:** `cargo test -p codegen`

**Done:** JIT module created. Empty fn compiled and called. VMContext layout matches expected offsets. Frame pointers preserved (RBP chain walkable). Stack map registry populated after compilation.

**Tests:**
- Create JIT module, compile empty fn, call it — returns without crash
- VMContext field offsets verified with `std::mem::offset_of!` (alloc_ptr at 0, alloc_limit at 8, gc_trigger at 16)
- Frame pointers preserved (compile fn, call it, verify RBP chain walkable from gc_trigger)
- Stack map registry populated with correct entries after compiling a fn with heap allocations
- **Stack map end-to-end verification:** compile a fn with 2+ known heap-ptr locals, call gc_trigger from that fn, verify the frame walker reads the *exact pointer values* from those SSA variables (not just "registry has entries"). This catches offset interpretation bugs.
- Alloc fast-path IR snippet: allocates object, bumps pointer

**Boundary:**
- Pin Cranelift version. Do not use latest — stack map API is unstable across versions.
- `preserve_frame_pointers = true` is REQUIRED. Without it, GC frame walking silently fails.
- VMContext is `#[repr(C, align(16))]`. Field order is frozen — gc_trigger reads it by offset.

**Gate:** TL reviews Cranelift setup before codegen workers spawn.

---

## Wave 2: Expression Codegen + Case/Join (2 workers, parallel)

### codegen-expr

**Task:** Core algebra: `CoreFrame<cranelift::Value>` → Cranelift IR for expressions.

**Read First:**
- `codegen/src/pipeline.rs` (compilation pipeline from scaffold)
- `codegen/src/context.rs` (VMContext)
- `codegen/src/alloc.rs` (alloc fast-path)
- `core-repr/src/frame.rs` (CoreFrame variants)
- `core-eval/src/eval.rs` (interpreter for oracle comparison)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `codegen/src/emit/expr.rs` — core expression emitter:
   - Lit → `iconst`/`fconst`
   - Var → SSA lookup from environment
   - Lam → flat closure alloc via fast-path (captured vars stored contiguously after header)
   - App → call with tail convention + vmctx
   - LetNonRec → lazy: thunk alloc via fast-path
   - LetRec → allocate all thunks, back-patch (same protocol as interpreter)
   - Con → alloc + store tag byte + store fields contiguously
2. Create `codegen/src/emit/primop.rs` — PrimOp variants → native Cranelift instructions:
   - Arithmetic: `+#`, `-#`, `*#` → `iadd`, `isub`, `imul`
   - Comparison: `==#`, `/=#`, `<#`, `>#` → `icmp`
   - Float: `+##`, `-##` → `fadd`, `fsub`
3. `declare_value_needs_stack_map` on EVERY SSA value that's a heap pointer
4. Error paths: unknown Var → proper error (not trap), unsupported PrimOp → clear error message

**Verify:** `cargo test -p codegen -- expr`

**Done:** All expression variants emit IR. Compiled results agree with interpreter oracle.

**Tests:**
- Lit roundtrip: `42` compiled → called → returns 42
- Identity: `(λx.x) 42 → 42`
- Closure captures free var: `let y = 1 in (λx. x + y) 2 → 3`
- Con with 3 fields at correct offsets
- PrimOp `+#` on two ints → correct result
- All tests also run through interpreter oracle: `compiled(e) == interpreted(e)`

**Boundary:**
- Do NOT manually force strict Con fields. Post-simplifier Core already has explicit case expressions.
- EVERY heap-ptr SSA value needs `declare_value_needs_stack_map`. Missing one = silent heap corruption.
- Thunk allocation uses the same protocol as the interpreter (Unevaluated → BlackHole → Evaluated).

---

### codegen-case-and-join

**Task:** Case dispatch via `br_table` and join points as parameterized Cranelift blocks.

**Read First:**
- `codegen/src/emit/expr.rs` (expression emitter)
- `core-repr/src/frame.rs` (Case, Alt, Join, Jump)
- `core-eval/src/eval.rs` (interpreter case/join logic for oracle)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `codegen/src/emit/case.rs` — Case dispatch:
   - Load tag byte from HeapObject at offset 0
   - `br_table` dispatch to alt blocks
   - Constructor alts: create block per alt, bind pattern vars via load at known HeapObject field offsets
   - Literal alts: compare value (not tag), branch accordingly
   - Default alt: catch-all block
2. Create `codegen/src/emit/join.rs` — Join/Jump:
   - Join → define parameterized Cranelift block, emit rhs as block body
   - Jump → `jump` instruction with args to the target block
   - Nested joins: inner blocks shadow outer labels
3. **Block param re-declaration (critical):** Every block param carrying a heap pointer MUST have `declare_value_needs_stack_map` called on it. This includes:
   - Case alt blocks receiving scrutinee/fields
   - Join point block params
   - Default alt block params

**Verify:** `cargo test -p codegen -- case join`

**Done:** Case dispatch correct for constructors, literals, defaults. Join/Jump works. All agree with interpreter.

**Tests:**
- 3-variant enum dispatches correctly (each alt reached)
- Default alt catches unmatched tag
- Field binding correct: `case (,) 1 2 of { (,) a b → a + b } → 3`
- Join with 2 params: both bound correctly on jump
- Nested case-of-case
- All tests: `compiled(e) == interpreted(e)`

**Boundary:**
- Block params are NEW SSA values. Re-declare `declare_value_needs_stack_map` at every block boundary. This is the #1 GC correctness hazard.
- `br_table` for case dispatch. Do NOT generate a chain of `brif` comparisons.
- Join points = parameterized blocks. Label scope is lexical (inner shadows outer).

---

**After wave 2:** TL integrates expr + case/join. `cargo test -p codegen`. Commit.

---

## Wave 3: GC Integration + EffectMachine (2 workers, parallel)

### gc-integration

**Task:** Wire GC root scanning to Cranelift stack maps. Two parts: IR-time metadata + runtime frame walker.

**Read First:**
- `codegen/src/stack_map.rs` (StackMapRegistry from scaffold)
- `codegen/src/context.rs` (VMContext, gc_trigger)
- `core-heap/src/gc/trace.rs` (GC trace for root enumeration)
- `core-heap/src/gc/compact.rs` (GC compact for pointer rewriting)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. **Part A (IR-time):** Verify all emit modules call `declare_value_needs_stack_map` correctly:
   - Audit expr.rs, case.rs, join.rs for heap-ptr SSA values
   - Verify block params re-declared at every boundary
   - Cranelift handles safepoint spill/reload automatically
2. **Part B (runtime):** Create `codegen/src/gc/frame_walker.rs` — frame walker in `gc_trigger`:
   - Read own RBP, step up ONE frame to JIT caller
   - `caller_rbp = *(rbp)`, `caller_pc = *(rbp+8)`
   - Look up `caller_pc` in StackMapRegistry → StackMapInfo
   - Compute frame bottom: `rsp = rbp - frame_size`
   - For each offset: `root_addr = rsp + offset`, read `*mut HeapObject`
   - After GC: write forwarding addr back to each root_addr
   - Walk full chain: `next_rbp = *(rbp)`, terminate when `rbp == 0`
   - Skip frames without stack map entries (Rust host fns between JIT frames)
   - Bounds-check PC against JIT memory region
3. Create `codegen/src/gc/trigger.rs` — `gc_trigger` implementation:
   - Must be compiled with frame pointers (`force-frame-pointers = true` in profile)
   - Called FROM JIT code (extern "C" fn)
   - Collects roots via frame walker, triggers GC, rewrites forwarding pointers
4. If stack map API proves insufficient, document why and implement shadow stack as alternative

**Verify:** `cargo test -p codegen -- gc`

**Done:** Compiled code survives GC at safepoints. Compiled+GC agrees with interpreted+GC.

**Tests:**
- Compiled code survives GC at safepoints (allocate enough to trigger GC mid-eval)
- `compiled_with_gc(e) == interpreted_with_gc(e)` (proptest)
- Stress test: force GC during deep eval chain (1000+ nested calls)
- Frame walker terminates correctly with mixed JIT/host frames
- Root addresses all point within heap bounds

**Boundary:**
- Do NOT rewrite `code_ptr` during GC (code space, not heap).
- `gc_trigger` is called FROM JIT code. It reads RBP to walk the JIT stack. It MUST be compiled with frame pointers.
- EVERY heap-ptr SSA value needs stack map declaration. Audit all emit modules.
- Block params are NEW SSA values — re-declare at every boundary.

---

### codegen-yield

**Task:** EffectMachine: the Rust driver for compiled freer-simple effect stacks.

**Read First:**
- `tidepool-plans/decisions.md` (§freer-simple Architecture, §EffectMachine)
- `codegen/src/emit/expr.rs` (compiled evaluation)
- `core-eval/src/value.rs` (Value for oracle comparison)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `codegen/src/effect_machine.rs` — `EffectMachine` struct:
   - Owns: compiled function pointer, VMContext, HeapRef to current expr
   - `step()` → evaluate `current_expr` to WHNF, pattern match result tag:
     - `Pure val` → `Yield::Done(val)`
     - `Free (Send req cont)` → `Yield::Request(req)` (stash `cont`)
   - `resume(result)` → construct `App(cont, result)`, set as current_expr, loop
2. Create `codegen/src/effect_machine/yield_type.rs` — `Yield<Req, Val>` enum:
   - `Done(Val)`
   - `Request(Req)`
   - Error variants: `UnexpectedTag(u8)`, `EvalError(EvalError)`, `HeapExhausted`
3. Debug logging: log effect request/response sequence at trace level
4. `EffectMachine` must be `Send` (no `Rc`, no thread-local state in the struct itself)

**Context:**
The EffectMachine is a ~30-line `Iterator`-like wrapper. `step()` evaluates to WHNF, pattern-matches the tag. `resume(result)` constructs `App(cont, result)`. The continuation `cont` is already a heap-allocated closure — no manual continuation capture needed. GC runs between steps only (clean collection points).

**Verify:** `cargo test -p codegen -- effect_machine`

**Done:** EffectMachine drives compiled effect stacks. Multi-step sequences agree with interpreter.

**Tests:**
- Effect stack with 3+ different effect types: step/resume cycle completes
- Resume with wrong type → clean error (not crash)
- Empty stack (Pure immediately) → immediate `Yield::Done`
- 10-step sequence: `compiled_steps == interpreted_steps`
- `EffectMachine` is `Send` (compile-time assertion: `fn assert_send<T: Send>() {}; assert_send::<EffectMachine>()`)

**Boundary:**
- No manual continuation capture. The continuation is already a heap closure.
- GC only between steps. Do not trigger GC mid-step.
- EffectMachine must be Send. No Rc, no thread-local refs in the struct.

---

**After wave 3:** TL wires complete codegen pipeline: CoreExpr → optimize → Cranelift IR → native fn → EffectMachine. Property: `compiled(e) == interpreted(e)` (proptest 1M+). Benchmark compiled vs interpreted. `cargo test -p codegen`. Commit. File PR.

---

## Domain Anti-Patterns

In addition to the base anti-patterns file:

- Do NOT manually force strict Con fields. Post-simplifier Core already handles this.
- EVERY heap-ptr SSA value needs `declare_value_needs_stack_map`. Missing one = silent heap corruption.
- Block params are NEW SSA values. Re-declare at every block boundary. This is the #1 GC correctness hazard.
- Do NOT rewrite `code_ptr` during GC (code space, not heap).
- Cranelift `br_table` for case dispatch. Do NOT generate a chain of `brif` comparisons.
- `gc_trigger` is called FROM JIT code. It reads RBP to walk the JIT stack. It MUST be compiled with frame pointers.
- No manual continuation capture in EffectMachine. The continuation is already a heap closure.
- Pin Cranelift version (prefer release tag over arbitrary commit). Stack map API is unstable across versions.
- HeapObject is a manual memory layout, not a Rust enum. See decisions.md.
- Frame walker terminates when PC leaves JIT region, not when `rbp == 0`.
- Stack map keys use *return addresses*, not call-site offsets.
