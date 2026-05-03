# Audit: effect_machine.rs

## Basic Con Accessors

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`read_con_tag`, `read_con_num_fields`, `read_con_field`)
- **Reads:** `u64` at `CON_TAG_OFFSET` (8), `u16` at `CON_NUM_FIELDS_OFFSET` (16), `*mut u8` at `CON_FIELDS_OFFSET + 8*i` (24+)
- **Expected shape:** `TAG_CON` header at offset 0.
- **Tag-check coverage:** no — note as a fragility (callers must verify `TAG_CON` before calling these low-level accessors)
- **Failure mode on shape mismatch:** `UB` (raw pointer reads from arbitrary offsets)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs` (exercised by almost all tests)
- **Notes:** paired with `alloc_con` and `tidepool_heap::layout::write_header`. Fragility: if passed a `TAG_LIT` or `TAG_THUNK`, it will read internal metadata as pointers/tags.

## parse_result: Result tag check

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`parse_result`)
- **Reads:** byte at `*result`
- **Expected shape:** Any valid `HeapObject`.
- **Tag-check coverage:** yes — verifies `tag == layout::TAG_CON`
- **Failure mode on shape mismatch:** `YieldError variant returned` (`YieldError::UnexpectedTag`)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_unexpected_tag`
- **Notes:** Defensive check ensuring the JIT function returned a `Con` as expected for the `Eff` type.

## parse_result: Val shape extraction

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`parse_result` dispatch)
- **Reads:** `con_tag` (8), `num_fields` (16), `fields[0]` (24)
- **Expected shape:** `TAG_CON` with `con_tag == self.tags.val` and `num_fields >= 1`.
- **Tag-check coverage:** yes — `read_con_tag` check follows the `TAG_CON` check.
- **Failure mode on shape mismatch:** `YieldError variant returned` (`YieldError::BadValFields` if `num_fields < 1`)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_yield_done_val`
- **Notes:** Writer: `codegen/src/emit/expr.rs` (Con emission). Pair: `tidepool-eval/src/eval.rs` (Value::Done).

## parse_result: E shape extraction

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`parse_result` dispatch)
- **Reads:** `num_fields` (16), `fields[0]` (24), `fields[1]` (32)
- **Expected shape:** `TAG_CON` with `con_tag == self.tags.e` and `num_fields == 2`.
- **Tag-check coverage:** yes — part of the `con_tag` dispatch.
- **Failure mode on shape mismatch:** `YieldError variant returned` (`YieldError::BadEFields`)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_yield_request_e`
- **Notes:** Extracts `union_ptr` and `continuation`. Writer: `codegen/src/emit/expr.rs`.

## parse_result: Union object header/field checks

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`parse_result` union extraction)
- **Reads:** byte at `*union_ptr` (0), `num_fields` (16)
- **Expected shape:** `TAG_CON` with `num_fields == 2`.
- **Tag-check coverage:** yes — verifies `union_tag == layout::TAG_CON` and `union_num_fields == 2`.
- **Failure mode on shape mismatch:** `YieldError variant returned` (`YieldError::UnexpectedTag` or `YieldError::BadUnionFields`)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_yield_request_e`
- **Notes:** The Union wrapper is produced by `freer-simple`'s `E` constructor.

## parse_result: Union position-tag boxing branch

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`parse_result` union tag read)
- **Reads:** byte at `*tag_ptr` (0), then either `LIT_VALUE_OFFSET` (16) or `field[0]` (24) of boxed `Con`.
- **Expected shape:** Either `TAG_LIT` (unboxed `Word#`) or `TAG_CON` (boxed `W# n`).
- **Tag-check coverage:** yes — branch on `tag_ptr_tag == layout::TAG_LIT` vs `layout::TAG_CON`.
- **Failure mode on shape mismatch:** `YieldError variant returned` (`YieldError::UnexpectedTag`)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_yield_request_e_boxed_tag`
- **Notes:** Recent fix in PR #272. Necessary for cross-module compilation where GHC might not unbox the effect tag.

## parse_result: Union request extraction

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`parse_result` union request extraction)
- **Reads:** `fields[1]` (32) of Union object.
- **Expected shape:** `TAG_CON` (Union) with at least 2 fields.
- **Tag-check coverage:** yes — depends on the `union_num_fields == 2` check.
- **Failure mode on shape mismatch:** `UB` (if `union_num_fields` check were bypassed)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_yield_request_e`
- **Notes:** Extracts the actual effect request object.

## force_ptr: Thunk tag check

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`force_ptr`)
- **Reads:** byte at `*current` (0)
- **Expected shape:** Any valid `HeapObject`.
- **Tag-check coverage:** yes — verifies `tag == layout::TAG_THUNK`.
- **Failure mode on shape mismatch:** `silent fallback: returns current`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs` (via lazy fields)
- **Notes:** Transparently forces thunks. Crucial because `Con` fields in JIT-compiled code are often lazy.

## apply_cont_heap: Continuation tag check

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`apply_cont_heap`)
- **Reads:** byte at `*k` (0)
- **Expected shape:** `TAG_CON` (Leaf/Node) or `TAG_CLOSURE`.
- **Tag-check coverage:** yes — `match tag` covers `TAG_CON` and `TAG_CLOSURE`.
- **Failure mode on shape mismatch:** `silent fallback: returns null_mut` (logged via `push_diagnostic`)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_resume_leaf_identity`
- **Notes:** Writer: `tidepool-eval/src/eval.rs` or `alloc_con` in `effect_machine.rs`.

## apply_cont_heap: Leaf/Node con_tag dispatch

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`apply_cont_heap` loop)
- **Reads:** `con_tag` (8), then `field[0]` (24) for Leaf, or `field[0], field[1]` for Node.
- **Expected shape:** `TAG_CON` with `con_tag` being `leaf` (arity 1) or `node` (arity 2).
- **Tag-check coverage:** yes — follows `tag == layout::TAG_CON` check.
- **Failure mode on shape mismatch:** `silent fallback: returns null_mut` (diagnostic)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_resume_node_identity`
- **Notes:** Iterative work-stack tree walking. `Leaf` contains a closure; `Node` contains two continuations.

## apply_cont_heap: Closure tag check

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`apply_cont_heap` closure arm)
- **Reads:** none (already read `tag`)
- **Expected shape:** `TAG_CLOSURE`.
- **Tag-check coverage:** yes — part of the `match tag` dispatch.
- **Failure mode on shape mismatch:** `silent fallback: returns null_mut` (diagnostic)
- **Mode:** `always-on`
- **Test coverage:** `uncovered`
- **Notes:** Degenerate case where a raw closure is used as a continuation.

## apply_cont_heap: Result tag/con_tag check

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`apply_cont_heap` result check)
- **Reads:** byte at `*result` (0), `con_tag` (8)
- **Expected shape:** `TAG_CON` with `con_tag == val` or `e`.
- **Tag-check coverage:** yes — verifies `result_tag == layout::TAG_CON`.
- **Failure mode on shape mismatch:** `silent fallback: returns null_mut` (diagnostic)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs`
- **Notes:** Verifies the result of a continuation application is a valid `Eff` value.

## apply_cont_heap: Val(y) composition

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`apply_cont_heap` Val arm)
- **Reads:** `field[0]` (24) of `Val` result.
- **Expected shape:** `TAG_CON` with `con_tag == val`.
- **Tag-check coverage:** yes — dispatch on `result_con_tag == self.tags.val`.
- **Failure mode on shape mismatch:** `UB` (if dispatch logic were faulty)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_resume_node_identity`
- **Notes:** Intermediate `Val` results in a `Node` chain trigger the next continuation.

## apply_cont_heap: E(union, k') composition

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`apply_cont_heap` E arm)
- **Reads:** `field[0]` (24) and `field[1]` (32) of `E` result.
- **Expected shape:** `TAG_CON` with `con_tag == e`.
- **Tag-check coverage:** yes — dispatch on `result_con_tag == self.tags.e`.
- **Failure mode on shape mismatch:** `UB`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_resume_node_with_effect_result`
- **Notes:** Composition of effectful results. Re-wraps the union and composes the remaining continuation stack.

## call_closure: Code pointer read

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`call_closure`)
- **Reads:** `usize` at `CLOSURE_CODE_PTR_OFFSET` (8)
- **Expected shape:** `TAG_CLOSURE` header.
- **Tag-check coverage:** no — fragility (callers must ensure `closure` is a `TAG_CLOSURE`).
- **Failure mode on shape mismatch:** `UB` (jumps to arbitrary address read from heap)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs:test_resume_leaf_identity`
- **Notes:** Critical path. Writer: `codegen/src/emit/expr.rs`. Pair: `heap_bridge.rs`.

## call_closure: Captured fields read

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`call_closure` tracing)
- **Reads:** `u16` at `CLOSURE_NUM_CAPTURED_OFFSET` (16), `*const u8` at `CLOSURE_CAPTURED_OFFSET + 8*i` (24+)
- **Expected shape:** `TAG_CLOSURE` with `num_captured` correctly set.
- **Tag-check coverage:** no — fragility.
- **Failure mode on shape mismatch:** `UB` (tracing-only, but could crash during debug log)
- **Mode:** `mode-dependent: debug/trace`
- **Test coverage:** `uncovered` (requires trace level >= Heap)
- **Notes:** Only used for tracing/validation in `call_closure`.

## resolve_tail_calls: Cancellation safepoint

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`resolve_tail_calls`)
- **Reads:** `VMContext.tail_callee` (24), `VMContext.tail_arg` (32)
- **Expected shape:** Non-null `tail_callee` implies a pending tail call.
- **Tag-check coverage:** no — fragility (assumes JIT set valid heap pointers).
- **Failure mode on shape mismatch:** `UB` (eventual crash in `call_closure`)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/external_cancellation.rs`
- **Notes:** The cancellation check `check_cancel_and_set_error` is a major safepoint.

## resolve_tail_calls: Code pointer read

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`resolve_tail_calls` loop)
- **Reads:** `usize` at `CLOSURE_CODE_PTR_OFFSET` (8) of `callee`.
- **Expected shape:** `TAG_CLOSURE` header on `callee`.
- **Tag-check coverage:** no — fragility.
- **Failure mode on shape mismatch:** `UB` (jump to garbage)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/external_cancellation.rs`
- **Notes:** Duplicates `call_closure` logic for the tail-call loop to avoid recursion.

## alloc_con: Heap layout write

- **Location:** `tidepool-codegen/src/effect_machine.rs` (`alloc_con`)
- **Reads:** none (write site)
- **Expected shape:** `TAG_CON` header, `con_tag`, `num_fields`, and `fields`.
- **Tag-check coverage:** n/a (it's the writer)
- **Failure mode on shape mismatch:** `UB` (if allocation size is miscalculated)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs` (via `resume` and `Node` composition)
- **Notes:** Primary producer of composed `Node` and `E` objects in the machine. Layout must stay in sync with `tidepool-heap`.

## Runtime error poison pointer

- **Location:** `tidepool-codegen/src/host_fns.rs:~164` (`error_poison_ptr`); referenced from `effect_machine.rs:~568`
- **Reads:** none (sentinel address)
- **Expected shape:** Constant non-null pointer (`0x45...ff`) used as a return value when JIT code must abort but cannot return null without segfaulting downstream readers.
- **Tag-check coverage:** n/a (sentinel)
- **Failure mode on shape mismatch:** Caller surfaces actual `RuntimeError` via `take_runtime_error()`.
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/host_fns.rs` unit tests
- **Notes:** Pre-allocated 16 KiB Closure-shaped buffer (sized in PR #272). When JIT code mistakenly reads the poison's tag byte it sees `TAG_CLOSURE` and treats it as a benign degenerate closure — no segfault.

## Union object field count

- **Location:** `tidepool-codegen/src/effect_machine.rs:~217` (Union arity guard in `parse_result`)
- **Reads:** `Self::read_con_num_fields(union_ptr)`
- **Expected shape:** `Union` constructor must have exactly 2 fields: tag (position index) at field 0, request payload at field 1.
- **Tag-check coverage:** yes — returns `Yield::Error(BadUnionFields(n))` if not 2.
- **Failure mode on shape mismatch:** `BadUnionFields` yield error.
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs::test_yield_request_e_boxed_tag`
- **Notes:** freer-simple invariant; any future change to the `Union` shape must update both this guard and the JIT emitter.

## Boxed Union tag — runtime fallback

- **Location:** `tidepool-codegen/src/effect_machine.rs:~245` (Union tag peeling in `parse_result`)
- **Reads:** `tag_ptr_tag` (TAG_LIT or TAG_CON), then field 0 of inner `W#` Con if boxed.
- **Expected shape (post-#289 normalization):** `Lit(LitWord, n)` directly; ideally Rule 2 unboxes.
- **Expected shape (fallback):** `Con(W#, [Lit(LitWord, n)])` for cross-module variables that Rule 2 cannot safely unbox.
- **Tag-check coverage:** yes — `UnexpectedTag` if neither TAG_LIT nor TAG_CON.
- **Failure mode on shape mismatch:** `UnexpectedTag(tag)` yield error.
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/effect_machine.rs::test_yield_request_e_boxed_tag`, `tidepool-runtime/tests/cross_mode_targeted.rs::dimension_a1_single_gadt_dispatch`
- **Notes:** PR #289 added the canonicalization rule and removed this fallback; PR #293 restored it per the principle (debug_assert only fires on irrecoverable wrongness; cross-module Var-resolved boxing is a valid runtime input). See `tidepool-repr/src/normalize.rs` Rule 2.

## k2_stack GC root registration

- **Location:** `tidepool-codegen/src/effect_machine.rs:~353` (`apply_cont_heap` k2_stack push)
- **Reads:** `k2_stack` slot pointers
- **Expected shape:** Each pushed `k2` is a heap pointer to a continuation `Con` (`Leaf` or `Node`).
- **Tag-check coverage:** indirect — the next `apply_cont_heap` iteration tag-checks the popped pointer.
- **Failure mode on shape mismatch:** GC may move objects and invalidate stack entries if not registered as roots.
- **Mode:** `always-on`
- **Test coverage:** GC stress tests in `tidepool-codegen/tests/gc_audit.rs`
- **Notes:** Must call `register_rust_root` before `call_closure` (which can trigger GC) and `unregister_rust_root` after. Failure to register would cause use-after-move bugs surfacing as null pointers or garbage tags.

## Continuation shape (Leaf | Node | Closure fallback)

- **Location:** `tidepool-codegen/src/effect_machine.rs:~388` (continuation `con_tag` switch in `apply_cont_heap`)
- **Reads:** `con_tag` from continuation
- **Expected shape:** `con_tag == self.tags.leaf` (for `Leaf(f)`) or `self.tags.node` (for `Node(k1, k2)`); raw `TAG_CLOSURE` is also accepted as a degenerate continuation called directly.
- **Tag-check coverage:** yes — emits `runtime_error_with_msg` on unexpected tag (PR #295 hardening).
- **Failure mode on shape mismatch:** Recoverable: surfaced as `RuntimeError::UserError` via `take_runtime_error()`.
- **Mode:** `always-on`
- **Test coverage:** Indirectly via `tidepool-runtime/tests/cross_mode_*` cross-module effect programs
- **Notes:** Hardened in PR #295 from `debug_assert!(false, ...)` to a recoverable error path.

## Eff result shape (Val | E)

- **Location:** `tidepool-codegen/src/effect_machine.rs:~440-478` (post-resume result inspection in `apply_cont_heap`)
- **Reads:** Result `tag` (must be `TAG_CON`), result `con_tag` (must be `Val` or `E`).
- **Expected shape:** Top-level `Eff a` value as `Val(a)` (terminal) or `E(union, k')` (yields request, awaits resumption).
- **Tag-check coverage:** yes — both checks emit `runtime_error_with_msg` on unexpected.
- **Failure mode on shape mismatch:** Recoverable: surfaced via `take_runtime_error()`.
- **Mode:** `always-on`
- **Test coverage:** Indirectly via every `tidepool-runtime/tests/*effect*` test
- **Notes:** Hardened in PR #295 from `debug_assert!(false, ...)` (which surfaced as silent null returns in release).

## call_closure: Captured fields read (debug)

- **Location:** `tidepool-codegen/src/effect_machine.rs:~518` (debug/trace helper)
- **Reads:** `num_captured` at offset 16, captured pointers from offset 24
- **Expected shape:** `TAG_CLOSURE` header with `code_ptr` at offset 8, `num_captured` at offset 16, `[*const u8; num_captured]` at offset 24+.
- **Tag-check coverage:** no — debug-trace only; assumes caller already verified the closure shape.
- **Failure mode on shape mismatch:** Garbage in trace output; no runtime impact.
- **Mode:** `cfg(debug_assertions)` or `TIDEPOOL_TRACE_*` env-gated
- **Test coverage:** none
- **Notes:** Read-only diagnostic; harmless in release builds.
