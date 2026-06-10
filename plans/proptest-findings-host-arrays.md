# Proptest findings — host array & double host functions (W7 host-arrays)

Stateful op-sequence property testing of the previously-untested ByteArray,
boxed-array (incl. CAS), and Double decode/show host functions in
`tidepool-codegen/src/host_fns.rs`. Companion test:
`tidepool-codegen/tests/proptest_host_arrays.rs`.

## Driver route (chosen)

**Direct extern-C host-fn calls**, matching the existing `proptest_host_fns.rs`
precedent — not hand-built CoreExpr PrimOp trees.

The PrimOps exist (`NewByteArray`, `CopyByteArray`, `ShrinkMutableByteArray`,
`ResizeMutableByteArray`, `CompareByteArrays`, `NewArray`, `CloneArray`, `Cas*`,
…), but driving them through a CoreExpr requires threading `State# RealWorld`
tokens, materialising boxed `MutableByteArray#` Lit values, and decoding unboxed
result tuples — impractical and itself a source of test bugs. Direct calls give
exact control over offsets, lengths, fenceposts, and overlapping ranges, which
is the whole point. The boundary explicitly permits this "where a CoreExpr route
is impractical."

## Method

- **Generator:** `Vec<ArrOp>` / `Vec<BoxOp>` (len 1..40), interpreted twice — once
  against the real host fns (raw pointers), once against a Rust model
  (`Vec<u8>` for bytes, `Vec<i64>` for boxed slots). Full-state equivalence is
  checked after **every** op (stronger than the required after-every-Compare),
  plus per-op runtime-error-flag cleanliness (B2) and Compare-result equality (B1).
- **Fenceposts:** New/offset/len sizes biased to `{0,1,7,8,63,64,65,4095,4096}`
  (alignment + 8-byte-word + 4 KiB-page boundaries) plus random.
- **Overlap:** `Copy` ops can target the same array with intersecting ranges
  (memmove-vs-memcpy territory) — exercised deliberately.
- **Fork everything (B3):** every executing case runs in a `libc::fork` child on
  an 8 MB-stack thread; a fatal signal (SIGSEGV/SIGILL/SIGBUS) → parent
  `waitpid` sees `WIFSIGNALED` → shrinkable failure. Logical divergences are
  reported over a pipe and likewise shrink.
- **B4 / GcPoint substitution:** these buffers are `std::alloc`-managed, NOT
  GC-traced (zero references in `gc.rs`), so a nursery GC physically cannot
  relocate a ByteArray. The "tiny-nursery 4 KB A/B" oracle is therefore N/A for
  the direct-call route. It is replaced by **run-the-same-sequence-twice
  determinism** with `GcPoint` allocator-churn interleaved — which targets the
  real bug class for malloc'd buffers (use-after-free after `resize`'s free,
  allocator reuse, uninitialised-tail reads). `resize` uses `alloc_zeroed`, so
  this passes today; the oracle guards against a regression to `alloc`.

## Property suite

| Property | Cases | Oracle |
|---|---|---|
| `bytearray_model` | 300 | model equivalence + signal hunt |
| `boxed_array_model` | 200 | model equivalence incl. CAS + signal hunt |
| `bytearray_gc_run_twice` | 200 | run-twice determinism under GcPoint churn (B4) |
| `double_decode_show` | 500 | show determinism + pinned canonical + decode sentinels/invariants + decode/encode identity |
| `fencepost_and_overlap_coverage` | — | counter-asserts coverage thresholds |
| `bug1_show_double_scientific_decimal` | 500 | `#[ignore]`d — asserts Haskell sci-mantissa invariant (BUG-1) |

Suite is **GREEN**: 9 passed, 2 ignored (the BUG-1 repros).

## Fencepost / overlap coverage (counter-asserted)

`fencepost_and_overlap_coverage` samples 1500 sequences and asserts each
fencepost New-size is hit ≥ 20× and overlapping copies are ≥ 10 % of all copies.
Observed in a representative run:

- Fencepost New-size hits: `0`→369, `1`→372, `7`→403, `8`→374, `63`→363,
  `64`→369, `65`→375, `4095`→357, `4096`→356 — all ≫ 20.
- Copies: 7423 total, 1591 overlapping = **21.4 %** ≫ 10 %.

## Bugs found

| ID | Class | Host fn | Observed | Expected | Status |
|---|---|---|---|---|---|
| BUG-1 | B1 (contract mismatch) | `runtime_show_double_addr` / `haskell_show_double` | `show(1) = "5e-324"`, `show(1e10) = "1e10"` | `"5.0e-324"`, `"1.0e10"` | repro + seed committed, `#[ignore]`d |
| BUG-2 | Latent memory-safety (UB) | `runtime_resize_byte_array` | `dealloc` of a shrunk-then-resized array uses layout `8 + logical_len`, not the true backing size | `dealloc` with the original alloc layout | inspection finding (allocator-tolerated; not oracle-triggered) |

### BUG-1 — scientific-notation Double `show` omits the mantissa decimal point

`haskell_show_double` (host_fns.rs ~1893) formats `|x| >= 1e7` and `|x| < 0.1`
via Rust `format!("{:e}", d)`. Rust renders a single-digit mantissa without a
decimal point (`1e10`, `5e-324`, `2e8`, `-1e10`), whereas Haskell's
`show :: Double -> String` **always** writes a mantissa decimal point
(`1.0e10`, `5.0e-324`, `2.0e8`, `-1.0e10`). The function's doc comment claims it
"matches Haskell's `show` output", so this is a contract (B1) divergence.

- **Oracle:** `sci_mantissa_violation` — if `show(bits)` is scientific (contains
  `e`/`E`; `Infinity`/`NaN` are not), its mantissa must contain `.`. This is
  grounded in the documented Haskell invariant, **not** in Rust `format!`
  equality (per the boundary).
- **Shrunk witness:** `bits = 1` (smallest positive subnormal, `5e-324`).
- **Human witness:** `1e10` → `"1e10"`.
- **Repros:** `bug1_show_double_scientific_decimal` (property) and
  `bug1_repro_minimal` (deterministic), both `#[ignore]`d so the suite stays
  green; remove `#[ignore]` after the fix.
- **Seed:** committed in
  `tidepool-codegen/tests/proptest_host_arrays.proptest-regressions`
  (`cc 7388…` → `bits = 1`).
- **Fix direction (not applied):** emit a Haskell-faithful scientific formatter
  (ensure the mantissa always has a `.0` when it would otherwise be a bare
  integer), or reuse the existing decimal-path `.0`-append logic for the
  scientific branch.

### BUG-2 — `resize` deallocates with the wrong layout after a `shrink` (latent)

`runtime_shrink_byte_array` updates only the `[u64 len]` prefix; the backing
allocation keeps its original size. `runtime_resize_byte_array` then reads that
(now-shrunk) prefix as `old_size` and frees the old buffer with
`Layout::from_size_align(8 + old_size, 8)`. For a `new(N) → shrink(M<N) →
resize(K)` sequence, the `dealloc` layout (`8 + M`) does not match the layout the
buffer was allocated with (`8 + N`) — a violation of Rust's allocator contract
(UB).

- **Observability:** the default system allocator (glibc `free`) ignores the
  size argument, so the suite's content/run-twice oracles do **not** surface it
  under glibc; it would be caught by a size-checking allocator (Miri, a
  sanitizer, or `malloc` with size-class assertions). Reported here as a
  code-inspection finding, honestly outside the oracle's reach.
- **Note:** the test harness itself avoids the same hazard by tracking each
  array's true backing size separately from its logical length and freeing with
  the original backing size.
- **Fix direction (not applied):** track the true backing size (e.g. a second
  prefix word) and use it for the `resize` `dealloc`, or never shrink the
  backing such that `resize`'s `old_size` read is wrong.

## Not-bugs confirmed correct

- Overlapping `copyByteArray#` / `copyMutableByteArray#`
  (`runtime_copy_byte_array`, `runtime_copy_boxed_array`) use `ptr::copy`
  (memmove) and matched the model on all overlapping ranges.
- Out-of-bounds / negative offsets on set/copy/compare/shrink silently no-op
  (matching GHC) — model mirrors, no error flag set (B2 clean).
- `runtime_cas_boxed_array` returns the prior value and swaps iff `old ==
  expected` — exact match across 200×≤40 op sequences.
- `decodeDouble_Int64#` (`runtime_decode_double_*`): `(0,0)` for `0.0`/NaN,
  `(±1,0)` for `±Inf`; for finite nonzero, mantissa is odd, `|mantissa| ≤ 2^53`,
  and `mantissa·2^exp == d` exactly in the `[1e-200, 1e200]` band.
- `runtime_resize_byte_array` zero-fills the grown tail (`alloc_zeroed`) and
  preserves `min(old,new)` bytes — content-correct (the BUG-2 defect is purely
  in the `dealloc` layout, invisible to content checks).
