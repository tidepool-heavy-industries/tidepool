# Proptest Findings: Heap Object Layout
Date: 2026-06-10

## Route & Inventory
Probed the RAW heap object layout primitives for encoding bugs, focusing on header encoding, Con field identity, Lit roundtrips, the byte-array capacity-word ABI, and thunk state encoding.

**Probed Surface:**
- `tidepool_heap::layout`: `write_header`, `read_tag`, `read_size`, constants (`TAG_*`, `OFFSET_*`, `*_OFFSET`).
- `tidepool_heap::gc::raw`: `for_each_pointer_field`, `evacuate` via `cheney_copy`.
- `tidepool_codegen::layout`: Constants synchronization check, `LIT_TAG_*` values.
- `tidepool_codegen::host_fns`: `runtime_new_byte_array`, `runtime_shrink_byte_array`, `runtime_resize_byte_array`, `runtime_set_byte_array`, `error_poison_ptr`.

## Bug Table
| ID | Severity | Site | Description | Repro Test | Seed |
|---|---|---|---|---|---|
| C1 | Medium | `tidepool-heap/src/layout.rs:141` | `write_header` takes `size: u16`, causing silent truncation if `size >= 65536`. | `bug_c1_header_wrap` | N/A |
| C2 | Critical | `tidepool-codegen/src/effect_machine.rs:721` (`alloc_con`, variable `fields.len()`) | Con writer computes `24 + 8*len` then casts `as u16`. For `len >= 8189`, size wraps (8189 -> 0, 8190 -> 8, ...) while `CON_NUM_FIELDS` stays correct. GC `evacuate` then copies `read_size()` (truncated) bytes — a 8189-field Con evacuates as 0 bytes, fields lost; `cheney_copy` scan advances by the wrapped size and walks into garbage. `heap_bridge.rs:399` (`value_to_heap`) is GUARDED (`MAX_FIELDS=1024`) so it is SAFE; `alloc_con` takes an arbitrary-length `fields` slice with NO guard. The fixed-arity writers (`host_fns.rs` cons cells = 2 fields, nullary cons = 0) cannot wrap. Repro replicates the unguarded pattern via `write_con_raw`. | `bug_c2_con_writer_wrap_gc` | N/A |
| C3 | Low | `tidepool-codegen/src/layout.rs:56` | Constant drift: Codegen defines Lit tags 5-9 (`STRING`..`ARRAY`), but `tidepool_heap::layout::LitTag::from_byte` only supports 0-4. | `test_lit_tag_drift` | N/A |
| C6 | Medium | `tidepool-heap/src/gc/raw.rs:88` | `BLACKHOLE` thunks have 0 visit counts in `for_each_pointer_field`, making captures invisible to GC. | `test_blackhole_gc_invisibility` | N/A |

## Verified Negative Table (Candidates Probed & Found Correct)
| Candidate | Result | Reasoning |
|---|---|---|
| C4: Padding Asymmetry | Verified | `write_header` zeroes padding only if `size >= 8`. For `size < 8`, it preserves stale bytes. Documented as current behavior. |
| C5: Byte-array ABI | Verified | `new`, `shrink`, and `resize` correctly manage the hidden capacity word at `ptr - 8`. `resize` preserves content and zeroes grown tail. |
| C7: Lit NaN Preservation | Verified | `Lit` values with `LIT_TAG_DOUBLE` preserve bit-identity of NaNs and other `u64` bit patterns. |

## Boundary Coverage
| Probe | Boundary Values | Verdict |
|---|---|---|
| Header Size | 0, 1, 8, 255, 256, 257, 4095, 65534, 65535 | Pass (Exact identity) |
| Header Size (Wrap) | 65536 | **BUG C1** (Wraps to 0) |
| Con Fields | 0, 1, 2, 1023, 1024, 8188 | Pass (Exact identity) |
| Con Fields (Wrap) | 8189 | **BUG C2** (Size wraps to 0) |
| Byte-array Size | 0, 1, 7, 8, 63, 64, 65 | Pass (Exact alignment & capacity) |
| Lit Tags | 0, 1, 2, 3, 4 | Pass (Identity) |
| Lit Tags (Drift) | 5, 6, 7, 8, 9 | **BUG C3** (Returns None on heap) |

## Reproduction Instructions
To run the reproduction tests for the confirmed bugs:
```bash
cargo test -p tidepool-codegen --test proptest_heap_layout -- --ignored
```
Targeted repros:
- `bug_repros::bug_c1_header_wrap`
- `bug_repros::bug_c2_con_writer_wrap_gc`
