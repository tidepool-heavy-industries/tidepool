# Audit: heap_bridge.rs

## Null pointer guard

- **Location:** `tidepool-codegen/src/heap_bridge.rs:76` (`heap_to_value_inner`)
- **Reads:** `ptr`
- **Expected shape:** Non-null pointer to a `HeapObject`
- **Decoded into:** N/A (guard)
- **Failure mode on shape mismatch:** `BridgeError::NullPointer`
- **Bound checks:** `ptr.is_null()`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:556` (`test_null_pointer_error`)
- **Notes:** Fundamental safety check before any offset-based reads.

## Recursion depth guard

- **Location:** `tidepool-codegen/src/heap_bridge.rs:79` (`heap_to_value_inner`)
- **Reads:** `depth` (recursion parameter)
- **Expected shape:** Object graph depth within safety limits
- **Decoded into:** N/A (guard)
- **Failure mode on shape mismatch:** `BridgeError::TooDeep`
- **Bound checks:** `MAX_DEPTH` (10,000)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/heap_bridge_tests.rs:64` (`test_heap_to_value_deeply_nested_cons`)
- **Notes:** Prevents stack overflow when decoding circular or extremely deep structures (e.g. large linked lists).

## LitTag dispatch

- **Location:** `tidepool-codegen/src/heap_bridge.rs:86` (`heap_to_value_inner`)
- **Reads:** `*ptr.add(layout::LIT_TAG_OFFSET as usize)`
- **Expected shape:** `TAG_LIT` header, followed by a valid `LitTag` at offset 8
- **Decoded into:** N/A (dispatch)
- **Failure mode on shape mismatch:** `BridgeError::UnexpectedLitTag`
- **Bound checks:** None
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:364` (and many others)
- **Notes:** Primary entry point for all primitive decoding.

## LitTag::Int

- **Location:** `tidepool-codegen/src/heap_bridge.rs:90` (`heap_to_value_inner`)
- **Reads:** `*(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64)`
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_INT` (0); 64-bit integer at offset 16
- **Decoded into:** `Value::Lit(Literal::LitInt(_))`
- **Failure mode on shape mismatch:** `BridgeError::UnexpectedLitTag` (if tag mismatches)
- **Bound checks:** None
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:364`, `tidepool-codegen/tests/heap_bridge_tests.rs:10`
- **Notes:** Assumes bit-representation of Haskell `Int#` matches Rust `i64`.

## LitTag::Word

- **Location:** `tidepool-codegen/src/heap_bridge.rs:91` (`heap_to_value_inner`)
- **Reads:** `*(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64)`
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_WORD` (1); 64-bit word at offset 16
- **Decoded into:** `Value::Lit(Literal::LitWord(_))`
- **Failure mode on shape mismatch:** `BridgeError::UnexpectedLitTag`
- **Bound checks:** None
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:378`
- **Notes:** Interprets the `i64` heap value as `u64`.

## LitTag::Char

- **Location:** `tidepool-codegen/src/heap_bridge.rs:92` (`heap_to_value_inner`)
- **Reads:** `*(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64)`
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_CHAR` (2); 32-bit Unicode scalar zero-extended to 64-bit at offset 16
- **Decoded into:** `Value::Lit(Literal::LitChar(_))`
- **Failure mode on shape mismatch:** `silent fallback: char::from_u32(...).unwrap_or('\0')`
- **Bound checks:** None
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:392`
- **Notes:** Silent fallback to NUL if the heap contains invalid Unicode data.

## LitTag::Float / LitTag::Double

- **Location:** `tidepool-codegen/src/heap_bridge.rs:95` (`heap_to_value_inner`)
- **Reads:** `*(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64)`
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_FLOAT` (3) or `LIT_TAG_DOUBLE` (4); raw bits at offset 16
- **Decoded into:** `Value::Lit(Literal::LitFloat(_))` | `Value::Lit(Literal::LitDouble(_))`
- **Failure mode on shape mismatch:** `BridgeError::UnexpectedLitTag`
- **Bound checks:** None
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:406` (Double), `tidepool-codegen/src/heap_bridge.rs:541` (Float)
- **Notes:** IEEE 754 bits are passed through as `u64`.

## LitTag::String

- **Location:** `tidepool-codegen/src/heap_bridge.rs:97` (`heap_to_value_inner`)
- **Reads:** `raw_value` as `*const u8` pointer; `std::ptr::read_unaligned(data_ptr as *const u64)` for length
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_STRING` (5). Pointer at offset 16 targets a buffer: `[len: u64][bytes...]`
- **Decoded into:** `Value::Lit(Literal::LitString(_))`
- **Failure mode on shape mismatch:** `BridgeError::NullPointer` (if data pointer is null) | `BridgeError::DataTooLarge` (if len > 64MB)
- **Bound checks:** `MAX_DATA_SIZE` (64MB)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:419`
- **Notes:** Uses `read_unaligned` because string data in JIT data sections may lack 8-byte alignment.

## LitTag::Addr fallback

- **Location:** `tidepool-codegen/src/heap_bridge.rs:112` (`heap_to_value_inner`)
- **Reads:** Tag only
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_ADDR` (6)
- **Decoded into:** `Value::Lit(Literal::LitString(vec![]))`
- **Failure mode on shape mismatch:** `intentional fallback: produces empty LitString`
- **Bound checks:** None
- **Mode:** `always-on` — `Addr#` is a legitimate intermediate runtime value emitted by primops (`PlusAddr`, `ShowDoubleAddr`; see `tidepool-codegen/src/emit/primop.rs` for `SsaVal::Raw(_, LIT_TAG_ADDR)` sites).
- **Test coverage:** `uncovered`
- **Notes:** Not a defensive guard against translator bugs. The bridge can't decode a raw pointer back to a typed Haskell value (no length, no type tag), so empty `LitString` is the safe display fallback when an `Addr#` is the top-level result. Programs that compose `Addr#` with `unpackCString#` etc. don't hit this path because they evaluate to a real string before reaching the bridge.

## LitTag::ByteArray

- **Location:** `tidepool-codegen/src/heap_bridge.rs:117` (`heap_to_value_inner`)
- **Reads:** `raw_value` as `*const u8` pointer; length from `ba_ptr`
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_BYTEARRAY` (7). Pointer at offset 16 targets a malloc-allocated buffer: `[len: u64][bytes...]`
- **Decoded into:** `Value::ByteArray(_)` (Arc<Mutex<Vec<u8>>>)
- **Failure mode on shape mismatch:** `silent fallback: Value::ByteArray(empty)` if pointer is null | `BridgeError::DataTooLarge`
- **Bound checks:** `MAX_DATA_SIZE` (64MB)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:502`
- **Notes:** `ByteArray#` data lives outside the GC nursery to prevent use-after- Cheney-copy bugs.

## LitTag::Array / LitTag::SmallArray

- **Location:** `tidepool-codegen/src/heap_bridge.rs` (match arm `LIT_TAG_SMALLARRAY` / `LIT_TAG_ARRAY` in `heap_to_value_inner`)
- **Reads:** `raw_value` as `*const u8` pointer; length from `arr_ptr`; field pointers from `arr_ptr.add(8 + 8*i)`
- **Expected shape:** `TAG_LIT` with `lit_tag = LIT_TAG_SMALLARRAY` (8) or `LIT_TAG_ARRAY` (9). Pointer targets: `[len: u64][ptr0][ptr1]...`
- **Decoded into:** `Value::Con(DataConId(0), elems)`
- **Failure mode on shape mismatch:** `BridgeError::NullPointer` if pointer is null (Hardened) | `BridgeError::DataTooLarge`
- **Bound checks:** `MAX_DATA_SIZE` (64MB), `MAX_DEPTH` (via recursion)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/tests/heap_bridge_tests.rs::test_heap_to_value_lit_smallarray_null`, `tidepool-codegen/tests/heap_bridge_tests.rs::test_heap_to_value_lit_array_null`
- **Notes:** Coerces boxed pointer arrays into a generic `Con(0, ...)` structure. Semantic meaning depends on the wrapping Haskell constructor. DataConId(0) is a deliberate sentinel meaning "raw boxed-pointer array".

## TAG_CON field decoding

- **Location:** `tidepool-codegen/src/heap_bridge.rs:160` (`heap_to_value_inner`)
- **Reads:** `con_tag` (u64) at `layout::CON_TAG_OFFSET` (8); `num_fields` (u16) at `layout::CON_NUM_FIELDS_OFFSET` (16); pointers at `layout::CON_FIELDS_OFFSET + 8*i` (24+)
- **Expected shape:** `TAG_CON` header, followed by DataConId, arity, and an array of pointers to other `HeapObject`s.
- **Decoded into:** `Value::Con(DataConId(con_tag), fields)`
- **Failure mode on shape mismatch:** `BridgeError::TooManyFields` (if arity > 1024) | `BridgeError` from recursive field decodes
- **Bound checks:** `MAX_FIELDS` (1024), `MAX_DEPTH`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:435`, `450`, `475`, `tidepool-codegen/tests/heap_bridge_tests.rs:27`
- **Notes:** Fundamental assumption: every field of a `Con` is a valid 8-byte aligned `HeapObject` pointer. Unboxed fields in a `Con` will cause a crash/UB during recursive decoding.

## TAG_THUNK evaluated (Indirection)

- **Location:** `tidepool-codegen/src/heap_bridge.rs:179` (`heap_to_value_inner`)
- **Reads:** `THUNK_STATE_OFFSET` (8); `THUNK_INDIRECTION_OFFSET` (16)
- **Expected shape:** `TAG_THUNK` with state `layout::THUNK_EVALUATED` (2). Offset 16 contains an indirection pointer to the result.
- **Decoded into:** `Value` (via recursion into the target)
- **Failure mode on shape mismatch:** `BridgeError` from recursion
- **Bound checks:** `MAX_DEPTH`
- **Mode:** `always-on`
- **Test coverage:** `uncovered`
- **Notes:** Follows the "D7" update pattern. Crucial for correctness when the bridge encounters an already-forced thunk.

## TAG_THUNK forcing

- **Location:** `tidepool-codegen/src/heap_bridge.rs:186` (`heap_to_value_inner`)
- **Reads:** `vmctx` parameter; calls `crate::host_fns::heap_force`
- **Expected shape:** `TAG_THUNK` in any unevaluated state
- **Decoded into:** `Value` (result of the forced computation)
- **Failure mode on shape mismatch:** `BridgeError::UnevaluatedThunk` (if forcing fails to progress)
- **Bound checks:** None
- **Mode:** `mode-dependent: requires non-null vmctx`
- **Test coverage:** `uncovered`
- **Notes:** The bridge triggers side-effects (JIT execution) when `heap_to_value_forcing` is used.

## TAG_THUNK failure states

- **Location:** `tidepool-codegen/src/heap_bridge.rs:195` (`heap_to_value_inner`)
- **Reads:** `THUNK_STATE_OFFSET` (8)
- **Expected shape:** `TAG_THUNK` with state `layout::THUNK_UNEVALUATED` (0) or `layout::THUNK_BLACKHOLE` (1)
- **Decoded into:** N/A (error)
- **Failure mode on shape mismatch:** `BridgeError::UnevaluatedThunk` | `BridgeError::BlackHole` | `BridgeError::UnknownThunkState`
- **Bound checks:** None
- **Mode:** `always-on` (when `vmctx` is null)
- **Test coverage:** `uncovered`
- **Notes:** `BlackHole` indicates a thunk that depends on its own result (infinite loop).

## TAG_CLOSURE rejected as top-level bridge result (Hardened)

- **Location:** `tidepool-codegen/src/heap_bridge.rs` (match arm `TAG_CLOSURE` in `heap_to_value_inner`)
- **Reads:** Tag only
- **Expected shape:** `TAG_CLOSURE` header
- **Decoded into:** N/A — surfaced as error
- **Failure mode on shape mismatch:** `BridgeError::UnexpectedHeapTag(TAG_CLOSURE)` (Hardened by PR #295)
- **Bound checks:** None
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs::tests::test_unexpected_heap_tag` (covers the surfaced-error path; prior dummy-closure fallback removed)
- **Notes:** Closures are opaque to the bridge and must not appear as a top-level decode result. A `TAG_CLOSURE` reaching this site indicates an unforced thunk leaked through to the bridge, which is a JIT bug. Previously masked by a dummy `Value::Closure(_, _, _)` fallback; now surfaces immediately. See `core-shapes.md §8`.

## Unexpected heap tag

- **Location:** `tidepool-codegen/src/heap_bridge.rs:211` (`heap_to_value_inner`)
- **Reads:** `tag` byte at offset 0
- **Expected shape:** One of `TAG_CLOSURE`, `TAG_THUNK`, `TAG_CON`, `TAG_LIT`
- **Decoded into:** N/A (error)
- **Failure mode on shape mismatch:** `BridgeError::UnexpectedHeapTag(tag)`
- **Bound checks:** None
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/heap_bridge.rs:563` (`test_invalid_heap_tag`)
- **Notes:** Catch-all for memory corruption or unsynchronized `tidepool-heap` / `tidepool-codegen` layout constants.

## MAX_FIELDS bound

- **Location:** `tidepool-codegen/src/heap_bridge.rs:65` (`MAX_FIELDS = 1024`)
- **Reads:** `num_fields` halfword at `CON_NUM_FIELDS_OFFSET`
- **Expected shape:** `num_fields <= MAX_FIELDS`
- **Decoded into:** N/A (bound check)
- **Failure mode on shape mismatch:** `BridgeError::TooManyFields { count }`
- **Bound checks:** Yes (this is the bound)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-codegen/src/host_fns.rs::tests::poison_buf_absorbs_max_con_write`
- **Notes:** Linked at compile time to `host_fns::POISON_BUF_SIZE` via `const _: () = assert!(...)` (PR #274) so future MAX_FIELDS bumps fail to build rather than regressing into OOM-path heap corruption.

## read_unaligned for heap data sections

- **Location:** `tidepool-codegen/src/heap_bridge.rs:99` (LitString), `:121` (ByteArray), `:138` (SmallArray/Array)
- **Reads:** `u64` length prefix at heap data section start
- **Expected shape:** `[len: u64][bytes...]` or `[len: u64][ptr0][ptr1]...`
- **Decoded into:** Value byte slice or `Value::Con(DataConId(0), [elems])`
- **Failure mode on shape mismatch:** `BridgeError::DataTooLarge` or `NullPointer`; UB if alignment was wrongly assumed
- **Bound checks:** Yes (`MAX_DATA_SIZE = 64 MiB`)
- **Mode:** `always-on`
- **Test coverage:** Indirect (any `Text`/string-returning Haskell program)
- **Notes:** Uses `std::ptr::read_unaligned` because JIT-emitted data sections aren't guaranteed to be 8-byte aligned. Native loads would trap on architectures with strict alignment.

## SmallArray# / Array# coercion to generic Con

- **Location:** `tidepool-codegen/src/heap_bridge.rs:131` (`heap_to_value_inner`, LIT_TAG_SMALLARRAY/ARRAY arm)
- **Reads:** `[len: u64][ptr0..ptrN-1]` from raw value pointer
- **Expected shape:** Boxed pointer array
- **Decoded into:** `Value::Con(DataConId(0), [elem0..])` — synthetic Con with sentinel ID
- **Failure mode on shape mismatch:** `silent fallback: produces synthetic Con`. Callers infer element semantics from the wrapping `Vector`/`Array` constructor name on the parent `Con`.
- **Bound checks:** `MAX_DATA_SIZE`
- **Mode:** `always-on`
- **Test coverage:** `uncovered`
- **Notes:** Type-erased decoding. Identified as a silent fallback in dossier Coverage Gaps; PR #295 didn't touch this path. Hardening candidate (tracked under code-hardening-wave2).

## Cross-mode harness: structural equivalence

- **Location:** `tidepool-runtime/tests/cross_mode_harness/structural_eq.rs:46` (`assert_equivalent`)
- **Reads:** Two `CoreExpr`s (single-module + split-module compilations of the same Haskell source)
- **Expected shape:** Same root structure modulo alpha-renaming. `VarId` / `JoinId` may differ; structural shape, literal values, and DataCon names+arities must match.
- **Decoded into:** `var_map: HashMap<VarId, VarId>` and `join_map: HashMap<JoinId, JoinId>` track binding pairs as the walk descends.
- **Failure mode on shape mismatch:** `panic!` in test with diagnostic naming the divergent node index, variant kinds, and DataCon names.
- **Bound checks:** Tree size must match exactly (`single.len() == split.len()`).
- **Mode:** test-only
- **Test coverage:** `tidepool-runtime/tests/cross_mode_tests.rs::harness_detects_obvious_divergence`
- **Notes:** Asymmetric walk (single → split). `Closure`, `ThunkRef`, `JoinCont` treated as opaque (skipped during comparison).

## Cross-mode harness: tiered DataCon match

- **Location:** `tidepool-runtime/tests/cross_mode_harness/structural_eq.rs:341` (`assert_table_compatible`)
- **Reads:** `DataCon` rows from both single and split tables
- **Expected shape:** Match by `qualified_name` first; fallback to `(name, rep_arity)` as stable cross-mode key when qualified names differ.
- **Decoded into:** Boolean equivalence per pair.
- **Failure mode on shape mismatch:** Comparator returns false; outer assertion panics with "DataCon name+arity mismatch".
- **Bound checks:** `field_bangs` must be identical (same strictness annotations expected across modes).
- **Mode:** test-only
- **Test coverage:** All `tidepool-runtime/tests/cross_mode_*` tests
- **Notes:** Mirrors the resilient lookup tier in `tidepool-bridge::get_resilient`. If a future cross-mode bug surfaces from `field_bangs` divergence, this guard will fire.
