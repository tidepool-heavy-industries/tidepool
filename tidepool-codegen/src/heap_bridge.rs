use crate::context::VMContext;
use crate::layout::{
    self, LIT_TAG_ADDR, LIT_TAG_ARRAY, LIT_TAG_BYTEARRAY, LIT_TAG_CHAR, LIT_TAG_DOUBLE,
    LIT_TAG_FLOAT, LIT_TAG_INT, LIT_TAG_SMALLARRAY, LIT_TAG_STRING, LIT_TAG_WORD,
};
use tidepool_eval::value::{Value, ValueFrame};
use tidepool_heap::layout as heap_layout;
use tidepool_repr::{DataConId, Literal};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("unexpected heap tag: {0}")]
    UnexpectedHeapTag(u8),
    #[error("unexpected lit tag: {0}")]
    UnexpectedLitTag(u8),
    /// A `Value` variant with no heap representation reached `value_to_heap`
    /// (only `Con`/`Lit`/`ByteArray` are convertible). Distinct from
    /// `UnexpectedHeapTag` — this is a Rust-side `Value`-shape error, not a
    /// bad byte read from the heap, and must never be conflated with the
    /// `TAG_FORWARDED` (255) sentinel.
    #[error("non-convertible Value (no heap representation)")]
    NonConvertibleValue,
    #[error("null pointer")]
    NullPointer,
    #[error("nursery exhausted")]
    NurseryExhausted,
    #[error("too many Con fields: {count}")]
    TooManyFields { count: usize },
    #[error("data too large: {len} bytes")]
    DataTooLarge { len: usize },
    #[error("heap structure too deep (>10000 levels)")]
    TooDeep,
    #[error("unevaluated thunk")]
    UnevaluatedThunk,
    #[error("blackhole (thunk forcing itself)")]
    BlackHole,
    #[error("unknown thunk state: {0}")]
    UnknownThunkState(u8),
    #[error("internal error: {0}")]
    InternalError(String),
}

/// Convert a heap-allocated object to a Value.
///
/// ## Null-vmctx invariant (temporal safety)
///
/// This calls `heap_to_value_inner` with a null `vmctx`, so it runs OUTSIDE
/// any machine: the forcing arms in `heap_to_value_inner`/`resolve_whnf` are
/// all gated on `!vmctx.is_null()`, so no thunk is forced here and therefore
/// **no GC can fire during this call**. That makes the `RootScope`/
/// `register_rust_root` calls on this path genuine no-ops (there is no
/// collection for them to protect against), not a skipped safety measure.
///
/// This is also safe ACROSS a later run's GC, not just during this call:
/// `heap_to_value`'s traversal returns a COMPLETE DEEP COPY — every leaf is
/// owned Rust data (`LitString`/`ByteArray` via `.to_vec()`; `Con`/array/spine
/// via recursively-bridged owned `Value`s) — so the returned `Value` retains
/// NO pointer into the JIT heap. A subsequent run's `gc_trigger`/`perform_gc`
/// has nothing here to relocate or dangle. (Session-retained heap values are
/// a SEPARATE mechanism — rooted via `PERSISTENT_ROOTS`, registered during
/// `OldSpace::tenure` which runs DURING a run with a non-null `vmctx` — so
/// this null-path no-op never touches them.) See
/// `heap_bridge_tests::null_vmctx_bridge_survives_later_gc` for the regression test.
///
/// # Safety
///
/// `ptr` must point to a valid HeapObject allocated by the JIT nursery.
pub unsafe fn heap_to_value(ptr: *const u8) -> Result<Value, BridgeError> {
    // SAFETY: Caller guarantees ptr is a valid HeapObject from the JIT nursery.
    heap_to_value_inner(ptr, 0, std::ptr::null_mut(), ClosurePolicy::Reject)
}

/// Convert a heap-allocated object to a Value, forcing any unevaluated thunks
/// encountered during traversal.
///
/// # Safety
///
/// `ptr` must point to a valid HeapObject allocated by the JIT nursery.
/// `vmctx` must point to a valid VMContext (required for forcing thunks).
pub unsafe fn heap_to_value_forcing(
    ptr: *const u8,
    vmctx: *mut VMContext,
) -> Result<Value, BridgeError> {
    // SAFETY: Caller guarantees ptr is a valid HeapObject and vmctx is a valid VMContext.
    heap_to_value_inner(ptr, 0, vmctx, ClosurePolicy::Reject)
}

/// The reserved `DataConId` a tolerant bridge (see [`heap_to_value_forcing_tolerant`])
/// substitutes for a `TAG_CLOSURE` heap object it declines to reject. A
/// closure has no data representation — the JIT already lowered its body to
/// machine code — so it cannot be re-materialized as a `Value`. Under the
/// tolerant policy the bridge emits this childless sentinel `Con` in its place,
/// letting the SURROUNDING structure bridge (e.g. `FinalizeWith(site, closure)`
/// bridges `site` and the classifier reads it, while the closure field is only
/// a placeholder). The REAL closure stays live in the JIT heap and is applied
/// by REFERENCE (self-iterating-harness W4), never through this `Value`.
///
/// `u64::MAX` is deliberately out of the extract's `DataConId` range, so a
/// consumer that inspects the bridged value can tell a placeholder apart from
/// a genuine constructor.
pub const CLOSURE_SENTINEL: DataConId = DataConId(u64::MAX);

/// Deep scan: whether `v` — or anything nested inside a `Con`'s fields — is
/// the [`CLOSURE_SENTINEL`] placeholder. The ONE construction site
/// ([`heap_to_value_inner`]'s `ClosurePolicy::Substitute` arm, below) always
/// builds it with zero fields, so checking the constructor id alone is the
/// correct, invariant-backed predicate — an additional `fields.is_empty()`
/// check would be redundant, never load-bearing (pinned by
/// `sentinel_is_id_only_no_fields_check_needed` below).
pub fn contains_closure_sentinel(v: &Value) -> bool {
    match v {
        Value::Con(id, fields) => {
            *id == CLOSURE_SENTINEL || fields.iter().any(contains_closure_sentinel)
        }
        _ => false,
    }
}

/// Whether `con`'s field `idx` carries the [`CLOSURE_SENTINEL`] placeholder,
/// scanned via [`contains_closure_sentinel`] — for a caller holding a
/// suspend request `Con` and checking one specific field (e.g.
/// `FinalizeWith`'s value field) rather than the whole value.
pub fn field_contains_closure_sentinel(con: &Value, idx: usize) -> bool {
    matches!(con, Value::Con(_, fields) if fields.get(idx).is_some_and(contains_closure_sentinel))
}

/// How the bridge treats a `TAG_CLOSURE` heap object it encounters.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClosurePolicy {
    /// The default: closures are opaque and must not appear as a bridge result
    /// (`core-shapes.md §8`). Errors with `UnexpectedHeapTag(TAG_CLOSURE)`.
    Reject,
    /// Substitute [`CLOSURE_SENTINEL`] for any `TAG_CLOSURE` object rather than
    /// erroring — the W4 finalize-by-reference path, where a finalized closure
    /// value crosses in-heap and only the surrounding metadata needs bridging.
    Substitute,
}

/// Like [`heap_to_value_forcing`], but a `TAG_CLOSURE` object encountered
/// anywhere in the traversal becomes a [`CLOSURE_SENTINEL`] placeholder Con
/// instead of an error. Used ONLY on the `Finalize` suspend path
/// (self-iterating-harness W4): the finalized value may be a closure, which has
/// no data `Value` representation, so it is passed by REFERENCE (the raw heap
/// pointer stays live in the suspended session, applied via `run_child`) while
/// this bridge produces a `Value` shell the classifier reads the leading `site`
/// field out of.
///
/// # Safety
///
/// `ptr` must point to a valid HeapObject allocated by the JIT nursery.
/// `vmctx` must point to a valid VMContext (required for forcing thunks).
pub unsafe fn heap_to_value_forcing_tolerant(
    ptr: *const u8,
    vmctx: *mut VMContext,
) -> Result<Value, BridgeError> {
    // SAFETY: Caller guarantees ptr is a valid HeapObject and vmctx is a valid VMContext.
    heap_to_value_inner(ptr, 0, vmctx, ClosurePolicy::Substitute)
}

const MAX_DEPTH: usize = 10_000;
/// Maximum number of fields the read-side decoder will accept on a single
/// `Con` heap object. The poison buffer in `host_fns` must be large enough
/// to absorb a worst-case Con write at this arity (see
/// `host_fns::POISON_BUF_SIZE` and the compile-time assertion there).
pub(crate) const MAX_FIELDS: usize = 1024;
const MAX_DATA_SIZE: usize = 64 * 1024 * 1024; // 64MB

/// RAII guard for scoped run-rooted-GC-root registration: truncates the
/// owning machine's run-scoped root registry back to its construction mark on
/// drop, covering early returns. pub(crate): the jit_machine drive loop roots
/// the continuation with it across GC-capable response materialization.
///
/// Reached via `vmctx.machine_state` (leaf 3's GC-cluster reach — see the
/// module doc on `host_fns::gc`), NOT `CURRENT_MACHINE`. `vmctx` may be null:
/// `heap_to_value`'s null-vmctx bridge path constructs a `RootScope` whose
/// mark/truncate calls are then no-ops — see the null-vmctx invariant on
/// `heap_to_value` below for why that is temporally safe.
pub(crate) struct RootScope(*mut VMContext, usize);
impl RootScope {
    /// # Safety
    /// If `vmctx` is non-null, it must point to a live `VMContext` for this
    /// scope's entire lifetime (until it drops).
    pub(crate) unsafe fn new(vmctx: *mut VMContext) -> Self {
        Self(vmctx, crate::host_fns::rust_roots_mark(vmctx))
    }
}
impl Drop for RootScope {
    fn drop(&mut self) {
        // SAFETY: `self.0` satisfied `RootScope::new`'s contract at
        // construction and this scope has not out-lived it.
        unsafe {
            crate::host_fns::truncate_rust_roots(self.0, self.1);
        }
    }
}

/// Resolve a heap pointer to WHNF: follow thunk indirections and (when a
/// vmctx is available) force unevaluated thunks. Mirrors the TAG_THUNK
/// branch of `heap_to_value_inner`; used by the iterative spine walk to
/// step through lazy effect-result tails. `heap_force` roots its own
/// argument, so the returned pointer is valid post-GC.
unsafe fn resolve_whnf(mut p: *const u8, vmctx: *mut VMContext) -> Result<*const u8, BridgeError> {
    loop {
        if p.is_null() {
            return Err(BridgeError::NullPointer);
        }
        if *p != layout::TAG_THUNK {
            return Ok(p);
        }
        let state = *p.add(layout::THUNK_STATE_OFFSET as usize);
        match state {
            layout::THUNK_EVALUATED => {
                p = *(p.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *const *const u8);
            }
            _ if !vmctx.is_null() => {
                let forced = crate::host_fns::heap_force(vmctx, p as *mut u8);
                if forced.is_null() || std::ptr::eq(forced, p) {
                    return Err(BridgeError::UnevaluatedThunk);
                }
                p = forced;
            }
            layout::THUNK_UNEVALUATED => return Err(BridgeError::UnevaluatedThunk),
            layout::THUNK_BLACKHOLE => return Err(BridgeError::BlackHole),
            other => return Err(BridgeError::UnknownThunkState(other)),
        }
    }
}

unsafe fn heap_to_value_inner(
    ptr: *const u8,
    depth: usize,
    vmctx: *mut VMContext,
    closures: ClosurePolicy,
) -> Result<Value, BridgeError> {
    // SAFETY: ptr is a valid HeapObject from the JIT nursery (checked non-null below).
    // All field reads use known layout offsets. Recursion depth is bounded by MAX_DEPTH.
    if ptr.is_null() {
        return Err(BridgeError::NullPointer);
    }
    if depth > MAX_DEPTH {
        return Err(BridgeError::TooDeep);
    }

    // GC safety: converting children can FORCE thunks (a lazy effect-result
    // tail materializes a whole chunk), which can collect — moving this
    // frame's object out from under its raw pointer. Every frame roots its
    // own `ptr`, so the entire ancestor chain stays valid across any force,
    // and per-field pointers are re-read from the (updated) parent after
    // each child conversion. For non-heap pointers (tests use stack
    // buffers) the collector's from-space range check skips the slot.
    let mut ptr = ptr;
    // SAFETY: vmctx is either null (no-op scope; see the null-vmctx
    // invariant above) or the live VMContext this traversal was called with.
    let _roots = unsafe { RootScope::new(vmctx) };
    // SAFETY: the slot lives until _roots drops at function exit.
    unsafe {
        crate::host_fns::register_rust_root(vmctx, &mut ptr as *mut *const u8 as *mut *mut u8);
    }

    // Converting ⊥ (a lazy poison closure) is a genuine demand: raise its
    // deferred error (with captured message) so the caller surfaces
    // UserErrorMsg instead of misreading the closure as data. The runtime
    // error flag set here is picked up by the post-bridge take_runtime_error
    // checks in jit_machine.
    if !vmctx.is_null() && crate::host_fns::is_lazy_poison(ptr) {
        crate::host_fns::raise_lazy_poison(vmctx, ptr as *mut u8);
        return Err(BridgeError::UnevaluatedThunk);
    }

    let tag = *ptr;
    match tag {
        t if t == layout::TAG_LIT => {
            let lit_tag = *ptr.add(layout::LIT_TAG_OFFSET as usize) as i64;
            let raw_value = *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64);

            match lit_tag {
                x if x == LIT_TAG_INT as i64 => Ok(Value::Lit(Literal::LitInt(raw_value))),
                x if x == LIT_TAG_WORD as i64 => Ok(Value::Lit(Literal::LitWord(raw_value as u64))),
                x if x == LIT_TAG_CHAR as i64 => {
                    // core-shapes.md §1: Char lit must hold valid Unicode codepoint.
                    // Fallback to \0 if invalid.
                    let c = char::from_u32(raw_value as u32);
                    #[cfg(debug_assertions)]
                    if c.is_none() {
                        eprintln!("[heap_bridge] diagnostic: invalid Unicode codepoint {:#x} in Char lit; falling back to \\0", raw_value);
                    }
                    Ok(Value::Lit(Literal::LitChar(c.unwrap_or('\0'))))
                }
                x if x == LIT_TAG_FLOAT as i64 => {
                    Ok(Value::Lit(Literal::LitFloat(raw_value as u64)))
                }
                x if x == LIT_TAG_DOUBLE as i64 => {
                    Ok(Value::Lit(Literal::LitDouble(raw_value as u64)))
                }
                x if x == LIT_TAG_STRING as i64 => {
                    // LitString# — raw pointer to [len: u64][bytes...]
                    let str_ptr = raw_value as *const u8;
                    if str_ptr.is_null() {
                        return Err(BridgeError::NullPointer);
                    }
                    let len = std::ptr::read_unaligned(str_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let bytes_ptr = str_ptr.add(8);
                    let bytes = std::slice::from_raw_parts(bytes_ptr, len).to_vec();
                    Ok(Value::Lit(Literal::LitString(bytes)))
                }
                x if x == LIT_TAG_ADDR as i64 => {
                    // Addr# is a legitimate intermediate runtime value: primops like
                    // PlusAddr (see emit/primop.rs) emits
                    // SsaVal::Raw(_, LIT_TAG_ADDR), and any program that returns the
                    // raw address through the bridge surfaces here. We can't decode
                    // it back to a typed Haskell value (it's a raw pointer with no
                    // length), so we render an empty LitString as the safe fallback.
                    // See core-shapes.md §1.
                    Ok(Value::Lit(Literal::LitString(vec![])))
                }
                x if x == LIT_TAG_BYTEARRAY as i64 => {
                    // ByteArray# — raw pointer to [len: u64][bytes...]
                    let ba_ptr = raw_value as *const u8;
                    if ba_ptr.is_null() {
                        // ByteArray# legitimately can be empty/null in some Haskell programs;
                        // returning empty is correct. See docs/core-shapes/audit-heap-bridge.md#littagbytearray.
                        return Ok(Value::ByteArray(std::sync::Arc::new(
                            std::sync::Mutex::new(vec![]),
                        )));
                    }
                    let len = std::ptr::read_unaligned(ba_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let bytes_ptr = ba_ptr.add(8);
                    let bytes = std::slice::from_raw_parts(bytes_ptr, len).to_vec();
                    Ok(Value::ByteArray(std::sync::Arc::new(
                        std::sync::Mutex::new(bytes),
                    )))
                }
                x if x == LIT_TAG_SMALLARRAY as i64 || x == LIT_TAG_ARRAY as i64 => {
                    // SmallArray# (8) / Array# (9) — boxed pointer arrays
                    // Layout: [u64 length][ptr0][ptr1]...[ptrN-1]
                    let arr_ptr = raw_value as *const u8;
                    if arr_ptr.is_null() {
                        return Err(BridgeError::NullPointer);
                    }
                    let len = std::ptr::read_unaligned(arr_ptr as *const u64) as usize;
                    if len > MAX_DATA_SIZE {
                        return Err(BridgeError::DataTooLarge { len });
                    }
                    let mut elems = Vec::with_capacity(len);
                    for i in 0..len {
                        // Re-derive the buffer pointer from the ROOTED Lit
                        // each iteration: converting an element can force
                        // (and collect), moving the payload buffer.
                        let arr_now = (*(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64))
                            as *const u8;
                        let elem_ptr = *(arr_now.add(8 + 8 * i) as *const *const u8);
                        elems.push(heap_to_value_inner(elem_ptr, depth + 1, vmctx, closures)?);
                    }
                    // SmallArray#/Array# carry no per-array DataConId. The wrapping Con (e.g.
                    // Vector's Array constructor) supplies type context to downstream consumers.
                    // DataConId(0) here is a deliberate sentinel meaning "raw boxed-pointer array".
                    // This is the contract documented in docs/core-shapes/audit-heap-bridge.md#littagarray--littagsmallarray.
                    Ok(Value::Con(DataConId(0), elems))
                }
                other => Err(BridgeError::UnexpectedLitTag(other as u8)),
            }
        }
        t if t == layout::TAG_CON => {
            let num_fields =
                unsafe { *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) }
                    as usize;
            if num_fields > MAX_FIELDS {
                return Err(BridgeError::TooManyFields { count: num_fields });
            }

            if num_fields == 2 {
                // Iterative spine conversion: 2-field Con chains (cons
                // lists) walk in a loop instead of per-cell recursion —
                // recursion costs ~2 stack frames per element and the depth
                // cap would reject long lists outright, while lazy effect
                // results make 10k+-element lists routine. Any 2-field
                // chain (nested pairs included) rebuilds structure-
                // identically, so no list semantics are assumed. Forcing a
                // lazy tail materializes a chunk and can GC: `cur` is
                // rooted, and head/tail pointers are re-read from the
                // (updated) cell after each child conversion.
                let mut elems: Vec<(u64, Value)> = Vec::new();
                let mut cur = ptr;
                // SAFETY: same contract as the outer scope's RootScope::new above.
                let _spine_roots = unsafe { RootScope::new(vmctx) };
                // SAFETY: the slot lives until _spine_roots drops.
                unsafe {
                    crate::host_fns::register_rust_root(
                        vmctx,
                        &mut cur as *mut *const u8 as *mut *mut u8,
                    );
                }
                loop {
                    if *cur != layout::TAG_CON {
                        break;
                    }
                    let nf =
                        *(cur.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) as usize;
                    if nf != 2 {
                        break;
                    }
                    let cell_tag = *(cur.add(layout::CON_TAG_OFFSET as usize) as *const u64);
                    let head = *(cur.add(layout::CON_FIELDS_OFFSET as usize) as *const *const u8);
                    let hv = heap_to_value_inner(head, depth + 1, vmctx, closures)?;
                    elems.push((cell_tag, hv));
                    // Re-read the tail AFTER the head conversion (which may
                    // have collected and moved this cell).
                    let tail =
                        *(cur.add(layout::CON_FIELDS_OFFSET as usize + 8) as *const *const u8);
                    cur = resolve_whnf(tail, vmctx)?;
                }
                // `cur` is the terminator: nil, a non-pair Con, a literal —
                // or a shape the recursion will reject with the right error.
                let mut acc = heap_to_value_inner(cur, depth + 1, vmctx, closures)?;
                while let Some((cell_tag, hv)) = elems.pop() {
                    acc = Value::Con(DataConId(cell_tag), vec![hv, acc]);
                }
                return Ok(acc);
            }

            let con_tag = unsafe { *(ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64) };
            let fields: Vec<_> = (0..num_fields)
                .map(|i| {
                    // Field pointers are read from the ROOTED parent per
                    // iteration: an earlier field's conversion may have
                    // forced (and collected), moving this Con.
                    let field_ptr =
                        *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * i) as *const *const u8);
                    heap_to_value_inner(field_ptr, depth + 1, vmctx, closures)
                })
                .collect::<Result<_, _>>()?;
            Ok(Value::Con(DataConId(con_tag), fields))
        }
        t if t == layout::TAG_THUNK => {
            let state = unsafe { *ptr.add(layout::THUNK_STATE_OFFSET as usize) };
            match state {
                layout::THUNK_EVALUATED => {
                    let target = unsafe {
                        *(ptr.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *const *const u8)
                    };
                    heap_to_value_inner(target, depth + 1, vmctx, closures)
                }
                _ if !vmctx.is_null() => {
                    let forced = crate::host_fns::heap_force(vmctx, ptr as *mut u8);
                    if !forced.is_null() && !std::ptr::eq(forced, ptr) {
                        heap_to_value_inner(forced as *const u8, depth + 1, vmctx, closures)
                    } else {
                        Err(BridgeError::UnevaluatedThunk)
                    }
                }
                layout::THUNK_UNEVALUATED => Err(BridgeError::UnevaluatedThunk),
                layout::THUNK_BLACKHOLE => Err(BridgeError::BlackHole),
                _ => Err(BridgeError::UnknownThunkState(state)),
            }
        }
        t if t == layout::TAG_CLOSURE => match closures {
            // core-shapes.md §8: Closures are opaque and should not appear as top-level bridge results.
            // If we hit one, it indicates an unforced thunk leaked through or an invalid shape.
            ClosurePolicy::Reject => Err(BridgeError::UnexpectedHeapTag(layout::TAG_CLOSURE)),
            // W4 finalize-by-reference: emit a placeholder Con so the SURROUNDING
            // structure bridges. The real closure stays live in the JIT heap and
            // is applied by reference, never through this `Value`.
            ClosurePolicy::Substitute => Ok(Value::Con(CLOSURE_SENTINEL, Vec::new())),
        },

        other => Err(BridgeError::UnexpectedHeapTag(other)),
    }
}

/// Convert a Value to a heap-allocated object via VMContext bump allocation.
///
/// Stack-safe: runs as a fallible hylomorphism (`recursion` crate) over
/// [`ValueFrame`] — arbitrarily deep bushy structures (nested JSON, tuple
/// towers) convert without consuming call stack. Children allocate before
/// parents; sibling allocation order is otherwise unobserved by any caller.
///
/// # Safety
///
/// `vmctx` must point to a valid VMContext with sufficient nursery space.
pub unsafe fn value_to_heap(val: &Value, vmctx: &mut VMContext) -> Result<*mut u8, BridgeError> {
    recursion::try_expand_and_collapse::<ValueFrame<'_, recursion::PartiallyApplied>, _, _, _>(
        val,
        |v: &Value| match v {
            Value::Con(..) | Value::Lit(_) | Value::ByteArray(_) => Ok(v.as_frame()),
            _ => Err(BridgeError::NonConvertibleValue),
        },
        |frame: ValueFrame<'_, *mut u8>| match frame {
            // SAFETY: expansion admits only convertible leaves; vmctx is the
            // caller's exclusive borrow with sufficient nursery space.
            ValueFrame::Leaf(value) => unsafe { leaf_to_heap(value, vmctx) },
            ValueFrame::ConFun(..) => Err(BridgeError::NonConvertibleValue),
            ValueFrame::Con(id, field_ptrs) => unsafe {
                // The header stores num_fields as u16; silently truncating
                // (`len as u16`) made a 65536-field Con roundtrip to ZERO
                // fields (proptest_boundary_roundtrip B5). Refuse cleanly at
                // the same MAX_FIELDS bound heap_to_value enforces on read.
                if field_ptrs.len() > MAX_FIELDS {
                    return Err(BridgeError::TooManyFields {
                        count: field_ptrs.len(),
                    });
                }
                let size = 24 + 8 * field_ptrs.len();
                let ptr = bump_alloc_from_vmctx(vmctx, size);
                if ptr.is_null() {
                    return Err(BridgeError::NurseryExhausted);
                }
                heap_layout::write_header(ptr, layout::TAG_CON, size as u32);
                *(ptr.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = id.0;
                *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) =
                    field_ptrs.len() as u16;
                for (i, fp) in field_ptrs.into_iter().enumerate() {
                    *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * i) as *mut *mut u8) = fp;
                }
                Ok(ptr)
            },
        },
    )
}

/// Allocate a childless `Value` (Lit / ByteArray) as a heap object.
///
/// # Safety
///
/// `vmctx` must point to a valid VMContext with a live nursery; `val` must
/// be a `Lit` or `ByteArray` variant.
unsafe fn leaf_to_heap(val: &Value, vmctx: &mut VMContext) -> Result<*mut u8, BridgeError> {
    match val {
        Value::Lit(lit) => {
            let ptr = bump_alloc_from_vmctx(vmctx, layout::LIT_TOTAL_SIZE as usize);
            if ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }
            heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u32);

            match lit {
                Literal::LitInt(n) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_INT as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = *n;
                }
                Literal::LitWord(n) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_WORD as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *n;
                }
                Literal::LitChar(c) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_CHAR as u8;
                    // Ensure the full 8-byte slot is written to avoid reading UB junk later
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *c as u32 as u64;
                }
                Literal::LitFloat(bits) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_FLOAT as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *bits;
                }
                Literal::LitDouble(bits) => {
                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_DOUBLE as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut u64) = *bits;
                }
                Literal::LitString(bytes) => {
                    // LitString stored as Lit with tag=5, value = ptr to [len: u64][bytes...]
                    // We allocate via the stable runtime allocator to avoid nursery movement.
                    let data_ptr =
                        crate::host_fns::runtime_new_byte_array(bytes.len() as i64) as *mut u8;
                    if data_ptr.is_null() {
                        return Err(BridgeError::NurseryExhausted);
                    }
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr.add(8), bytes.len());

                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_STRING as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = data_ptr as i64;
                }
                Literal::LitByteArray(bytes) => {
                    // Same [len: u64][bytes...] data section as LitString, but
                    // tagged BYTEARRAY so sizeofByteArray# reads the length and
                    // mpn/index ops read the limbs (no unpackCString# +8 skip).
                    // (eval lowers LitByteArray to Value::ByteArray, so this is a
                    // belt-and-suspenders path for any direct Lit conversion.)
                    let data_ptr =
                        crate::host_fns::runtime_new_byte_array(bytes.len() as i64) as *mut u8;
                    if data_ptr.is_null() {
                        return Err(BridgeError::NurseryExhausted);
                    }
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr.add(8), bytes.len());

                    *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_BYTEARRAY as u8;
                    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = data_ptr as i64;
                }
            }
            Ok(ptr)
        }
        Value::ByteArray(bytes) => {
            // ByteArray# stored as Lit with tag=7 (LIT_TAG_BYTEARRAY), value = ptr to [len: u64][bytes...]
            // The byte data buffer must be allocated outside the GC nursery (via malloc)
            // because GC doesn't track the interior pointer from the Lit wrapper to the
            // data buffer. Using bump_alloc would place data in the nursery; after a
            // Cheney copy, the Lit's data_ptr would point to stale fromspace memory.
            let bytes = bytes
                .lock()
                .map_err(|e| BridgeError::InternalError(format!("mutex poisoned: {e}")))?;
            let data_ptr = crate::host_fns::runtime_new_byte_array(bytes.len() as i64) as *mut u8;
            if data_ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr.add(8), bytes.len());

            let ptr = bump_alloc_from_vmctx(vmctx, layout::LIT_TOTAL_SIZE as usize);
            if ptr.is_null() {
                return Err(BridgeError::NurseryExhausted);
            }
            heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u32);
            *ptr.add(layout::LIT_TAG_OFFSET as usize) = LIT_TAG_BYTEARRAY as u8;
            *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = data_ptr as i64;
            Ok(ptr)
        }
        _ => Err(BridgeError::NonConvertibleValue),
    }
}

/// Bump-allocate from VMContext. Returns null if nursery is exhausted.
///
/// # Safety
///
/// `vmctx` must point to a valid VMContext with a live nursery.
pub unsafe fn bump_alloc_from_vmctx(vmctx: &mut VMContext, size: usize) -> *mut u8 {
    // SAFETY: Caller guarantees vmctx points to a valid VMContext with a live nursery.
    // alloc_ptr and alloc_limit delimit the available nursery region.
    // Align to 8 bytes
    let aligned_size = (size + 7) & !7;
    let ptr = vmctx.alloc_ptr;
    let new_ptr = ptr.add(aligned_size);
    if new_ptr as *const u8 > vmctx.alloc_limit {
        return std::ptr::null_mut();
    }
    vmctx.alloc_ptr = new_ptr;
    ptr
}

/// The one gc-trigger-then-retry policy for every nursery allocation on the
/// effect-response / stowed-continuation path: run `alloc` once; if `exhausted`
/// says the result reports nursery exhaustion, trigger exactly one collection
/// and run `alloc` a second time, returning whatever that second attempt
/// produces (success or a terminal exhaustion). `gc_trigger`'s collection
/// includes heap doubling up to the configured cap, so one retry is the
/// established sufficient policy — the risk this class of bug lives in is a
/// call site skipping the retry entirely or retrying without every live value
/// rooted, not needing more than one cycle.
///
/// Generic over the allocation's return shape (`Result<*mut u8, BridgeError>`
/// from `value_to_heap`, or a raw `*mut u8` where null means exhausted from
/// `bump_alloc_from_vmctx`/`host_alloc_gc`-style call sites) via the
/// `exhausted` predicate.
///
/// # Safety
/// `vmctx` must be a valid, live `VMContext` for the duration of both calls to
/// `alloc`. Every heap pointer the caller holds live ACROSS this call must
/// already be a registered GC root (`register_rust_root`/
/// `register_persistent_root`/`register_stowed_root`) — the retry's
/// collection can move it. This function performs no rooting of its own.
pub(crate) unsafe fn gc_retry<T>(
    vmctx: *mut VMContext,
    exhausted: impl Fn(&T) -> bool,
    mut alloc: impl FnMut() -> T,
) -> T {
    let first = alloc();
    if !exhausted(&first) {
        return first;
    }
    GC_RETRY_FIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    crate::host_fns::gc_trigger(vmctx);
    alloc()
}

/// Process-wide count of `gc_retry` exhaustion branches actually taken (the
/// first attempt reported nursery exhaustion, so a `gc_trigger` + second
/// attempt ran). Test-only observable: proves a retry-protected call site was
/// not just reached but genuinely exercised its retry, as opposed to the
/// first attempt happening to succeed. Not part of the public API.
static GC_RETRY_FIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test-only: how many times `gc_retry`'s exhaustion-and-retry branch has run
/// in this process. Not part of the public API.
#[doc(hidden)]
pub fn gc_retry_fired_count() -> usize {
    GC_RETRY_FIRED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Test-only: reset the `gc_retry` fired counter. Not part of the public API.
#[doc(hidden)]
pub fn reset_gc_retry_fired_count() {
    GC_RETRY_FIRED.store(0, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    // SAFETY: All unsafe blocks in tests call value_to_heap/heap_to_value with
    // nursery-backed VMContexts created by setup_vmctx. The nursery is kept alive
    // for the duration of each test, ensuring all heap pointers remain valid.
    use super::*;
    use crate::nursery::Nursery;
    use tidepool_repr::Literal;

    extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

    fn setup_vmctx(size: usize) -> (Nursery, VMContext) {
        let mut nursery = Nursery::new(size);
        let vmctx = nursery.make_vmctx(mock_gc_trigger);
        (nursery, vmctx)
    }

    #[test]
    fn value_to_heap_deep_tower_is_stack_safe() {
        // 50k-deep single-field Con tower converted on a 64 KiB thread:
        // the hylo-based conversion must not consume call stack per level,
        // and the Value's drop is iterative. Build the tower iteratively too,
        // obviously.
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let mut v = Value::Lit(Literal::LitInt(7));
                for _ in 0..50_000 {
                    v = Value::Con(DataConId(3), vec![v]);
                }
                // 50k cells × 32 bytes + leaf ≈ 1.6 MB
                let (_nursery, mut vmctx) = setup_vmctx(4 * 1024 * 1024);
                let ptr = unsafe { value_to_heap(&v, &mut vmctx) }.expect("conversion failed");
                assert!(!ptr.is_null());
                // Spot-check the top two levels.
                unsafe {
                    assert_eq!(*ptr, layout::TAG_CON);
                    let inner = *(ptr.add(layout::CON_FIELDS_OFFSET as usize) as *const *const u8);
                    assert_eq!(*inner, layout::TAG_CON);
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_lit_int_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Lit(Literal::LitInt(42));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitInt(n)) = back else {
                panic!("Expected LitInt, got {:?}", back);
            };
            assert_eq!(n, 42);
        }
    }

    #[test]
    fn test_lit_word_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Lit(Literal::LitWord(123));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitWord(n)) = back else {
                panic!("Expected LitWord, got {:?}", back);
            };
            assert_eq!(n, 123);
        }
    }

    #[test]
    fn test_lit_char_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let val = Value::Lit(Literal::LitChar('λ'));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitChar(c)) = back else {
                panic!("Expected LitChar, got {:?}", back);
            };
            assert_eq!(c, 'λ');
        }
    }

    #[test]
    fn test_lit_float_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let bits = 1.234f32.to_bits() as u64;
        let val = Value::Lit(Literal::LitFloat(bits));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitFloat(b)) = back else {
                panic!("Expected LitFloat, got {:?}", back);
            };
            assert_eq!(b, bits);
        }
    }

    #[test]
    fn test_lit_double_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let bits = 5.678f64.to_bits();
        let val = Value::Lit(Literal::LitDouble(bits));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitDouble(b)) = back else {
                panic!("Expected LitDouble, got {:?}", back);
            };
            assert_eq!(b, bits);
        }
    }

    #[test]
    fn test_lit_string_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(1024);
        let bytes = b"hello world".to_vec();
        let val = Value::Lit(Literal::LitString(bytes.clone()));
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Lit(Literal::LitString(ref b)) = back else {
                panic!("Expected LitString, got {:?}", back);
            };
            assert_eq!(*b, bytes);
        }
    }

    #[test]
    fn test_con_roundtrip() {
        let (_nursery, mut vmctx) = setup_vmctx(2048);
        let val = Value::Con(
            DataConId(42),
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::Lit(Literal::LitInt(2)),
            ],
        );
        unsafe {
            let ptr = value_to_heap(&val, &mut vmctx).expect("value_to_heap failed");
            let back = heap_to_value(ptr).expect("heap_to_value failed");
            let Value::Con(id, ref fields) = back else {
                panic!("Expected Con, got {:?}", back);
            };
            assert_eq!(id.0, 42);
            assert_eq!(fields.len(), 2);
            assert!(matches!(fields[0], Value::Lit(Literal::LitInt(1))));
            assert!(matches!(fields[1], Value::Lit(Literal::LitInt(2))));
        }
    }

    #[test]
    fn test_invalid_heap_tag() {
        let mut nursery = Nursery::new(1024);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        unsafe {
            let ptr = bump_alloc_from_vmctx(&mut vmctx, 8);
            *ptr = 0xFE; // Invalid tag
            let res = heap_to_value(ptr);
            assert!(matches!(res, Err(BridgeError::UnexpectedHeapTag(0xFE))));
        }
    }

    #[test]
    fn test_tag_closure_error() {
        let mut nursery = Nursery::new(1024);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        unsafe {
            let ptr = bump_alloc_from_vmctx(&mut vmctx, 8);
            *ptr = layout::TAG_CLOSURE;
            let res = heap_to_value(ptr);
            assert!(matches!(
                res,
                Err(BridgeError::UnexpectedHeapTag(layout::TAG_CLOSURE))
            ));
        }
    }

    /// W4: the tolerant bridge substitutes a `CLOSURE_SENTINEL` placeholder for a
    /// bare `TAG_CLOSURE` where the default bridge errors — so a
    /// `FinalizeWith(site, closure)` request bridges (leaving the closure live
    /// in-heap, applied by reference) instead of failing the whole suspend.
    #[test]
    fn tolerant_bridge_substitutes_closure_sentinel() {
        let mut nursery = Nursery::new(1024);
        let mut vmctx = nursery.make_vmctx(mock_gc_trigger);
        unsafe {
            let ptr = bump_alloc_from_vmctx(&mut vmctx, 8);
            *ptr = layout::TAG_CLOSURE;
            let res = heap_to_value_forcing_tolerant(ptr, &mut vmctx as *mut _)
                .expect("tolerant bridge must not error on a closure");
            assert!(
                matches!(res, Value::Con(id, ref fields) if id == CLOSURE_SENTINEL && fields.is_empty()),
                "expected a childless CLOSURE_SENTINEL Con, got {res:?}"
            );
        }
    }

    /// The tolerant bridge substitutes the sentinel ONLY for closures — an
    /// ordinary `Con(site, closure)` bridges its data field normally and the
    /// closure field to the sentinel, so the surrounding structure survives.
    #[test]
    fn tolerant_bridge_keeps_data_fields_alongside_a_closure() {
        let (_nursery, mut vmctx) = setup_vmctx(2048);
        unsafe {
            // Build Con(7, [I#-ish Lit(41), <closure>]) by hand: the data field
            // bridges, the closure field becomes the sentinel.
            let site = value_to_heap(&Value::Lit(Literal::LitInt(41)), &mut vmctx).unwrap();
            let closure = bump_alloc_from_vmctx(&mut vmctx, 8);
            *closure = layout::TAG_CLOSURE;
            let size = 24 + 8 * 2;
            let con = bump_alloc_from_vmctx(&mut vmctx, size);
            tidepool_heap::layout::write_header(con, layout::TAG_CON, size as u32);
            *(con.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = 7;
            *(con.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = 2;
            *(con.add(layout::CON_FIELDS_OFFSET as usize) as *mut *mut u8) = site;
            *(con.add(layout::CON_FIELDS_OFFSET as usize + 8) as *mut *mut u8) = closure;

            let res = heap_to_value_forcing_tolerant(con, &mut vmctx as *mut _)
                .expect("tolerant bridge must not error");
            let Value::Con(id, ref fields) = res else {
                panic!("expected a Con, got {res:?}");
            };
            assert_eq!(id.0, 7);
            assert!(matches!(fields[0], Value::Lit(Literal::LitInt(41))));
            assert!(
                matches!(&fields[1], Value::Con(cid, cf) if *cid == CLOSURE_SENTINEL && cf.is_empty()),
                "the closure field must be the sentinel, got {:?}",
                fields[1]
            );
        }
    }

    /// Pins the invariant `contains_closure_sentinel`/`field_contains_closure_sentinel`
    /// rely on: the sentinel is ALWAYS childless, so an id-only check is
    /// exactly as strong as an id-and-empty-fields check. Also covers the
    /// nested case (a closure inside a product field) and a non-match.
    #[test]
    fn sentinel_is_id_only_no_fields_check_needed() {
        let sentinel = Value::Con(CLOSURE_SENTINEL, Vec::new());
        assert!(contains_closure_sentinel(&sentinel));

        let nested = Value::Con(
            DataConId(7),
            vec![Value::Lit(Literal::LitInt(41)), sentinel],
        );
        assert!(contains_closure_sentinel(&nested));
        assert!(field_contains_closure_sentinel(&nested, 1));
        assert!(!field_contains_closure_sentinel(&nested, 0));

        let plain_data = Value::Con(DataConId(7), vec![Value::Lit(Literal::LitInt(41))]);
        assert!(!contains_closure_sentinel(&plain_data));
        assert!(!field_contains_closure_sentinel(&plain_data, 0));
    }
}
