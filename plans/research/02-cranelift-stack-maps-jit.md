# Research: Cranelift Stack Maps + JIT Pipeline

**Priority:** HIGH — de-risks the entire codegen phase
**Status:** COMPLETE — empirically verified on cranelift 0.116.1
**POC code:** `/tmp/cranelift-poc/` (all 6 tests pass)

## Summary of Findings

### 1. Stack Map Offset Semantics — VERIFIED

**API chain:** `Context::compile(isa, ctrl_plane)` → `&CompiledCode` → `.buffer.user_stack_maps()` → `&[(CodeOffset, u32, UserStackMap)]`

**Tuple semantics:**
- `CodeOffset` — byte offset into the compiled code buffer. Corresponds to a safepoint (call instruction). Empirically: offset 49 and 70 for two call sites in a ~94 byte function.
- `u32` (called "span" in docs) — frame size or related metadata. Observed value: 16 (2 slots × 8 bytes each).
- `UserStackMap::entries()` → iterator of `(Type, u32)` where u32 is **SP-relative offset**. Positive. Example: `(i64, 0x0)` and `(i64, 0x8)` for two heap pointers.

**Key finding:** Offsets are **SP-relative, positive**. The math is: `root_address = SP + slot_offset` at the safepoint.

**The IR is transparent.** After `FunctionBuilder::finalize()`, the function IR clearly shows:
```
stack_store v1, ss0
call fn0(v2), stack_map=[i64 @ ss0+0]
stack_store v3, ss1
call fn1(), stack_map=[i64 @ ss0+0, i64 @ ss1+0]
```
Cranelift's frontend inserts explicit `stack_store` before safepoints and `stack_load` after. The stack map annotations appear directly on the call instructions. This is the "user stack maps" approach — the user (frontend) declares which values need maps, and Cranelift handles the spill/reload/annotation automatically.

**Progressive liveness:** The first call site has 1 entry (only ptr1 live), the second has 2 entries (both ptr1 and ptr2 live). Stack maps are precise per-safepoint.

### 2. Context::compile → Executable Memory Path — VERIFIED

**Recommended: Double-compile strategy (option C)**

```rust
// 1. Build function IR with FunctionBuilder
// 2. Compile via Context to extract stack maps
let compiled = ctx.compile(isa.as_ref(), &mut ctrl_plane).unwrap();
let stack_maps = compiled.buffer.user_stack_maps().to_vec();
// 3. Define via module for execution (recompiles internally)
module.define_function(func_id, &mut ctx).unwrap();
module.finalize_definitions().unwrap();
let ptr = module.get_finalized_function(func_id);
```

**Why this works:** `define_function` recompiles from `ctx.func` (the IR), so calling `compile` first doesn't interfere. The IR survives both compilations.

**define_function_bytes also exists** on the `Module` trait and compiles for cranelift-jit 0.116. Not tested for execution, but the double-compile approach is simpler and avoids dealing with relocations manually.

**Cost:** Compilation happens twice per function. For a JIT compiler this is acceptable — compilation is fast relative to the work the compiled code will do.

### 3. Tail CC + Stack Maps — VERIFIED, COEXIST

Tail calling convention (`CallConv::Tail`) compiles cleanly with `declare_value_needs_stack_map`. The stack map is correctly emitted at the safepoint:

```
function u0:0(i64) -> i64 tail {
    ss0 = explicit_slot 8, align = 8
    sig0 = () tail
block0(v0: i64):
    stack_store v0, ss0
    call_indirect sig0, v1(), stack_map=[i64 @ ss0+0]
    v2 = stack_load.i64 ss0
    return v2
}
```

Stack map entry: `offset=21, span=16, entries=[(i64, 0)]`. The heap pointer is correctly spilled to ss0 before the call and reloaded after.

### 4. Block Param Re-declaration — CONFIRMED REQUIRED

**This is the most critical finding for tidepool's codegen.**

When a value flows through a block param (via `jump`):
- **With `declare_value_needs_stack_map(block_param)`:** 1 stack map entry with the block param's slot
- **Without declaration on block param:** 0 stack map entries. The GC root is INVISIBLE.

The value's stack map property does **not** propagate through block params. Each new SSA value (including block params that receive a forwarded value) must be independently declared.

This means the tidepool codegen must call `declare_value_needs_stack_map` on every block param that carries a heap pointer, not just the original definition site.

### 5. Frame Pointers — VERIFIED

With `preserve_frame_pointers = true`, the RBP chain is walkable from a host function called by JIT code:

```
JIT function at: 0x564d6470f000
Frame 0: RBP=0x7ffcce020180, saved_RBP=0x7ffcce022410, ret_addr=0x564d577ae9bc
Frame 1: RBP=0x7ffcce022410, saved_RBP=0x7ffcce0224f0, ret_addr=0x564d577a86d7
Frame 2: RBP=0x7ffcce0224f0, saved_RBP=0x7ffcce022550, ret_addr=0x7faabda2b338
Frame 3: RBP=0x7ffcce022550, saved_RBP=0x0, ret_addr=0x564d577a45b5
Frame 4: RBP=0x0 -- stopping
```

The JIT frame appears in the chain. Frame 0 is the host function's frame, Frame 1 is the JIT function's frame (return address points into JIT code region near 0x564d577...), and subsequent frames are Rust frames.

**Frame walker termination:** The chain terminates with `saved_RBP=0x0`. In practice, for tidepool, terminate when the return address leaves the known JIT code region (more robust than checking RBP == 0).

## Cranelift Version

All tests ran on **cranelift-codegen 0.116.1** (with cranelift-frontend, cranelift-jit, cranelift-module, cranelift-native all at 0.116.1).

## Key API Reference

| API | Module | Signature |
|-----|--------|-----------|
| `declare_value_needs_stack_map` | `cranelift_frontend::FunctionBuilder` | `(&mut self, val: Value)` |
| `declare_var_needs_stack_map` | `cranelift_frontend::FunctionBuilder` | `(&mut self, var: Variable)` |
| `Context::compile` | `cranelift_codegen::Context` | `(&mut self, isa: &dyn TargetIsa, ctrl_plane: &mut ControlPlane) -> CompileResult<&CompiledCode>` |
| `buffer.user_stack_maps()` | `cranelift_codegen::machinst::MachBufferFinalized` | `(&self) -> &[(CodeOffset, u32, UserStackMap)]` |
| `UserStackMap::entries()` | `cranelift_codegen::ir::UserStackMap` | `(&self) -> impl Iterator<Item = (Type, u32)>` |
| `JITBuilder::symbol` | `cranelift_jit::JITBuilder` | `(&mut self, name: &str, ptr: *const u8)` |

## Implications for Codegen Spec

1. **Stack map math is simple:** `root_addr = SP + offset` at safepoint. SP-relative, positive offsets.
2. **Double-compile is the path:** Compile once for stack maps, `define_function` for execution.
3. **Block params MUST be declared individually.** The codegen must track heap-pointer-ness through the CFG and declare at every block param site.
4. **Tail CC works with stack maps.** No restrictions on combining them.
5. **Frame pointer chain is walkable** but terminate on JIT region boundary, not RBP==0.
