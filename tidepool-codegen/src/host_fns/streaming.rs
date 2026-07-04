//! Lazy effect-result materialization.
//!
//! Large list-shaped effect responses are not converted to heap cells eagerly.
//! Instead the dispatcher parks the flattened elements in the running
//! machine's parked-stream registry (`MachineState`, reached here via
//! `current_machine()`) and responds with a single thunk whose code pointer
//! is the HOST function `lazy_list_chunk` (precedent: poison trampolines —
//! `heap_force` calls thunk entries through a transmute and cannot tell host
//! from JIT).
//! Forcing the tail materializes the next CHUNK elements and a fresh tail
//! thunk. `take k` over a huge glob materializes one chunk ever; full folds
//! stream chunks through the (growing) heap while consumed cells become
//! garbage. The captures are raw ints — GC's evacuation range-check skips
//! non-pointer words, same as the vmctx tail fields.

use crate::context::VMContext;
use crate::machine_state::current_machine;

use super::cancel::check_cancel_and_set_error;
use super::errors::{error_poison_ptr, push_diagnostic, runtime_error_with_msg, runtime_oom};
use super::gc::{
    gc_trigger, host_alloc_gc, register_rust_root, rust_roots_mark, truncate_rust_roots,
};

/// A parked effect-response stream: the element producer (the iterator IS
/// the cursor — no offset bookkeeping), the list constructor tags, and an
/// owned `DataConTable` for pull-time element conversion. Sequential,
/// exactly-once consumption is guaranteed structurally: tail thunk N is
/// unreachable until tail N−1 was forced, and `heap_force` memoizes.
pub(crate) struct ParkedStream {
    pub source: Box<dyn tidepool_effect::ValueSource>,
    pub cons_tag: u64,
    pub nil_tag: u64,
    /// Conversion table for pull-time `ToCore`. Pre-converted sources
    /// (dismantled spines) ignore it — park an empty table for those.
    pub table: tidepool_repr::DataConTable,
}

/// Registry key for a parked stream. A `#[repr(transparent)]` newtype over the
/// raw `u64` id so `MachineState`'s parked-stream map cannot be keyed by an
/// arbitrary integer. The id CARRIED through JIT tail thunks stays a raw
/// `u64` (that write is on the emit hot path); we wrap into `StreamId` only
/// at the registry boundary (`park_stream` insert + the pull/probe lookups).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct StreamId(pub u64);

/// Source over pre-converted Values (a dismantled `Response::Complete`
/// spine). The table argument is unused. Random-access: element values are
/// cheap to clone (Text payloads are Arc-shared), so element thunks defer
/// only the value_to_heap byte copies.
pub(crate) struct ReadySource {
    items: Vec<tidepool_eval::value::Value>,
    pos: usize,
}

impl ReadySource {
    pub(crate) fn new(items: Vec<tidepool_eval::value::Value>) -> Self {
        Self { items, pos: 0 }
    }
}

impl tidepool_effect::ValueSource for ReadySource {
    fn next_value(
        &mut self,
        _table: &tidepool_repr::DataConTable,
    ) -> Option<Result<tidepool_eval::value::Value, tidepool_bridge::BridgeError>> {
        let item = self.items.get(self.pos)?;
        self.pos += 1;
        Some(Ok(item.clone()))
    }

    fn len(&self) -> Option<usize> {
        Some(self.items.len())
    }

    fn get(
        &self,
        idx: usize,
        _table: &tidepool_repr::DataConTable,
    ) -> Option<Result<tidepool_eval::value::Value, tidepool_bridge::BridgeError>> {
        self.items.get(idx).map(|v| Ok(v.clone()))
    }
}

/// Park a response stream; returns the registry id carried by tail thunks.
/// The no-machine case can't happen on a real run (parking only occurs while
/// driving an effect step loop, with `CURRENT_MACHINE` installed for the
/// duration) — return the sentinel id 0 rather than panic.
pub(crate) fn park_stream(stream: ParkedStream) -> u64 {
    match unsafe { current_machine() } {
        Some(ms) => ms.park_stream(stream),
        None => 0,
    }
}

/// Allocate a host-code thunk with two raw u64 captures. Raw ints are safe
/// captures: the GC's evacuation range-check skips non-pointer words.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
unsafe fn alloc_host_thunk2(
    vmctx: *mut VMContext,
    code: unsafe extern "C" fn(*mut VMContext, *mut u8) -> *mut u8,
    cap0: u64,
    cap1: u64,
) -> *mut u8 {
    let size = tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 16;
    let p = host_alloc_gc(vmctx, size);
    if p.is_null() {
        return std::ptr::null_mut();
    }
    tidepool_heap::layout::write_header(p, tidepool_heap::layout::TAG_THUNK, size as u16);
    *p.add(tidepool_heap::layout::THUNK_STATE_OFFSET) = tidepool_heap::layout::THUNK_UNEVALUATED;
    *(p.add(tidepool_heap::layout::THUNK_CODE_PTR_OFFSET) as *mut usize) = code as usize;
    *(p.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET) as *mut u64) = cap0;
    *(p.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 8) as *mut u64) = cap1;
    p
}

/// Allocate a stream-tail thunk carrying (registry id, offset). Sequential
/// sources ignore the offset (the parked iterator is the cursor); indexed
/// sources use it as the next chunk's start index.
/// Returns null on OOM (caller converts to poison via runtime_oom).
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
pub(crate) unsafe fn alloc_stream_tail_thunk(
    vmctx: *mut VMContext,
    id: u64,
    offset: u64,
) -> *mut u8 {
    alloc_host_thunk2(vmctx, stream_chunk, id, offset)
}

/// Allocate an element thunk carrying (registry id, element index):
/// forcing it converts exactly that element of an indexed source,
/// memoized by the standard thunk indirection.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
unsafe fn alloc_element_thunk(vmctx: *mut VMContext, id: u64, idx: u64) -> *mut u8 {
    alloc_host_thunk2(vmctx, stream_element, id, idx)
}

/// Allocate a nullary constructor (e.g. nil). Returns null on OOM.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
unsafe fn alloc_nullary_con(vmctx: *mut VMContext, con_tag: u64) -> *mut u8 {
    let size = tidepool_heap::layout::CON_FIELDS_OFFSET;
    let p = host_alloc_gc(vmctx, size);
    if p.is_null() {
        return std::ptr::null_mut();
    }
    tidepool_heap::layout::write_header(p, tidepool_heap::layout::TAG_CON, size as u16);
    *(p.add(tidepool_heap::layout::CON_TAG_OFFSET) as *mut u64) = con_tag;
    *(p.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 0;
    p
}

/// Build cons cells back-to-front over `items`, linking `terminator` as the
/// final tail. GC-safe: the terminator and partial chain are RUST_ROOTS
/// registered; element conversion retries once after gc_trigger. Returns the
/// chain head, or a poison pointer with a runtime error set on failure.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed;
/// `terminator` must be a valid heap pointer.
unsafe fn build_cons_cells(
    vmctx: *mut VMContext,
    cons_tag: u64,
    items: &[tidepool_eval::value::Value],
    terminator: *mut u8,
) -> *mut u8 {
    let mark = rust_roots_mark();
    let mut tail: *mut u8 = terminator;
    register_rust_root(&mut tail as *mut *mut u8);

    // Build cells back-to-front so each cons links the already-built tail.
    let mut elem: *mut u8 = std::ptr::null_mut();
    register_rust_root(&mut elem as *mut *mut u8);
    for v in items.iter().rev() {
        elem = match crate::heap_bridge::value_to_heap(v, &mut *vmctx) {
            Ok(p) => p,
            Err(crate::heap_bridge::BridgeError::NurseryExhausted) => {
                gc_trigger(vmctx);
                match crate::heap_bridge::value_to_heap(v, &mut *vmctx) {
                    Ok(p) => p,
                    Err(_) => {
                        truncate_rust_roots(mark);
                        return runtime_oom();
                    }
                }
            }
            Err(_) => {
                truncate_rust_roots(mark);
                let msg = b"effect result: element conversion failed";
                return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
            }
        };
        let size = tidepool_heap::layout::CON_FIELDS_OFFSET + 16;
        let cell = host_alloc_gc(vmctx, size);
        if cell.is_null() {
            truncate_rust_roots(mark);
            return runtime_oom();
        }
        tidepool_heap::layout::write_header(cell, tidepool_heap::layout::TAG_CON, size as u16);
        *(cell.add(tidepool_heap::layout::CON_TAG_OFFSET) as *mut u64) = cons_tag;
        *(cell.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 2;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *mut *mut u8) = elem;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *mut *mut u8) = tail;
        tail = cell;
    }
    truncate_rust_roots(mark);
    tail
}

/// Eagerly materialize a whole flattened list as a heap cons chain,
/// iteratively — no recursion over the spine. (A deep spine's recursive
/// `value_to_heap` or recursive `Value` Drop overflows the host stack; the
/// fault lands outside signal protection and silently kills the eval
/// thread.) Used by the dispatch site when lazy results are disabled.
/// Returns the chain head, or a poison pointer with a runtime error set.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
pub(crate) unsafe fn materialize_cons_list(
    vmctx: *mut VMContext,
    cons_tag: u64,
    nil_tag: u64,
    items: &[tidepool_eval::value::Value],
) -> *mut u8 {
    let nil = alloc_nullary_con(vmctx, nil_tag);
    if nil.is_null() {
        return runtime_oom();
    }
    build_cons_cells(vmctx, cons_tag, items, nil)
}

/// Outcome of pulling one chunk from a parked stream (separated from the
/// heap work so the registry borrow is released before any allocation).
enum ChunkPull {
    Missing,
    Cancelled,
    Failed(String),
    Chunk {
        cons_tag: u64,
        nil_tag: u64,
        items: Vec<tidepool_eval::value::Value>,
        exhausted: bool,
    },
}

/// Thunk entry for stream tails: pull the next chunk of elements from the
/// parked source, materialize them as cons cells, terminated by nil or a
/// fresh tail thunk.
///
/// # Safety
/// Called by `heap_force` with a valid vmctx and a thunk allocated by
/// `alloc_stream_tail_thunk`.
unsafe extern "C" fn stream_chunk(vmctx: *mut VMContext, thunk: *mut u8) -> *mut u8 {
    const CHUNK: usize = 256;
    // Read the captures before any allocation (the thunk may move on GC; its
    // registered slot lives in heap_force's frame, not ours).
    let id = *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET) as *const u64);
    let offset =
        *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 8) as *const u64) as usize;

    // Indexed sources (random access, e.g. respond_list / dismantled
    // spines): build spine cells whose HEADS are per-element thunks —
    // forcing a head converts exactly one element; a fold that never
    // inspects heads (length) converts nothing. Registry lookups only;
    // no producer code runs, so no panic/cancel containment needed here.
    let indexed = current_machine().and_then(|ms| {
        ms.parked_stream_get(StreamId(id), |ps| {
            (ps.source.len(), ps.cons_tag, ps.nil_tag)
        })
    });
    let Some((src_len, cons_tag, nil_tag)) = indexed else {
        let msg = b"effect result stream: registry entry missing (stale continuation?)";
        return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
    };
    if let Some(len) = src_len {
        let end = (offset + CHUNK).min(len);
        // NOTE: the registry entry must outlive the LAST element force, not
        // just the last chunk — entries for indexed sources are dropped at
        // machine teardown (RegistryGuard), never on exhaustion.
        let terminator: *mut u8 = if end >= len {
            let p = alloc_nullary_con(vmctx, nil_tag);
            if p.is_null() {
                return runtime_oom();
            }
            p
        } else {
            let p = alloc_stream_tail_thunk(vmctx, id, end as u64);
            if p.is_null() {
                return runtime_oom();
            }
            p
        };
        return build_cons_cells_thunked(vmctx, cons_tag, id, offset..end, terminator);
    }

    // Sequential sources: pull and CONVERT a chunk of elements. Runs
    // arbitrary producer Rust — two containments:
    // - catch_unwind: a producer panic must not unwind across the JIT
    //   frames below us (UB) — convert to a runtime error instead.
    // - cancel safepoint per pull: a slow (e.g. IO-backed) producer must
    //   be interruptible like any other long-running evaluation.
    // No heap operations happen while the registry borrow is held.
    let pulled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        current_machine()
            .and_then(|ms| {
                ms.parked_stream_get_mut(StreamId(id), |ps| {
                    let mut items = Vec::with_capacity(CHUNK);
                    let mut exhausted = false;
                    while items.len() < CHUNK {
                        if check_cancel_and_set_error(vmctx) {
                            return ChunkPull::Cancelled;
                        }
                        match ps.source.next_value(&ps.table) {
                            Some(Ok(v)) => items.push(v),
                            Some(Err(e)) => {
                                return ChunkPull::Failed(format!(
                                    "stream element conversion failed: {e}"
                                ))
                            }
                            None => {
                                exhausted = true;
                                break;
                            }
                        }
                    }
                    ChunkPull::Chunk {
                        cons_tag: ps.cons_tag,
                        nil_tag: ps.nil_tag,
                        items,
                        exhausted,
                    }
                })
            })
            .unwrap_or(ChunkPull::Missing)
    }));

    let (cons_tag, nil_tag, items, exhausted) = match pulled {
        Ok(ChunkPull::Chunk {
            cons_tag,
            nil_tag,
            items,
            exhausted,
        }) => (cons_tag, nil_tag, items, exhausted),
        Ok(ChunkPull::Missing) => {
            let msg = b"effect result stream: registry entry missing (stale continuation?)";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Ok(ChunkPull::Cancelled) => {
            // check_cancel_and_set_error already set the runtime error.
            return error_poison_ptr();
        }
        Ok(ChunkPull::Failed(msg)) => {
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Err(panic) => {
            let what = panic
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            let msg = format!("effect result stream: producer panicked: {what}");
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    if exhausted {
        if let Some(ms) = current_machine() {
            ms.remove_parked_stream(StreamId(id));
        }
    }

    // Terminal: nil constructor or the next tail thunk.
    let terminator: *mut u8 = if exhausted {
        let p = alloc_nullary_con(vmctx, nil_tag);
        if p.is_null() {
            return runtime_oom();
        }
        p
    } else {
        let p = alloc_stream_tail_thunk(vmctx, id, 0);
        if p.is_null() {
            return runtime_oom();
        }
        p
    };
    build_cons_cells(vmctx, cons_tag, &items, terminator)
}

/// Build cons cells back-to-front whose heads are ELEMENT THUNKS over an
/// indexed source range. Only spine allocations happen here — no element
/// conversion, no byte copies.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed;
/// `terminator` must be a valid heap pointer.
unsafe fn build_cons_cells_thunked(
    vmctx: *mut VMContext,
    cons_tag: u64,
    id: u64,
    range: std::ops::Range<usize>,
    terminator: *mut u8,
) -> *mut u8 {
    let mark = rust_roots_mark();
    let mut tail: *mut u8 = terminator;
    register_rust_root(&mut tail as *mut *mut u8);

    let mut elem: *mut u8 = std::ptr::null_mut();
    register_rust_root(&mut elem as *mut *mut u8);
    for idx in range.rev() {
        elem = alloc_element_thunk(vmctx, id, idx as u64);
        if elem.is_null() {
            truncate_rust_roots(mark);
            return runtime_oom();
        }
        let size = tidepool_heap::layout::CON_FIELDS_OFFSET + 16;
        let cell = host_alloc_gc(vmctx, size);
        if cell.is_null() {
            truncate_rust_roots(mark);
            return runtime_oom();
        }
        tidepool_heap::layout::write_header(cell, tidepool_heap::layout::TAG_CON, size as u16);
        *(cell.add(tidepool_heap::layout::CON_TAG_OFFSET) as *mut u64) = cons_tag;
        *(cell.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 2;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *mut *mut u8) = elem;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *mut *mut u8) = tail;
        tail = cell;
    }
    truncate_rust_roots(mark);
    tail
}

/// Thunk entry for stream elements: convert exactly one element of an
/// indexed source. Memoization via the standard thunk indirection makes
/// this a copy-on-read view over the parked Rust data.
///
/// # Safety
/// Called by `heap_force` with a valid vmctx and a thunk allocated by
/// `alloc_element_thunk`.
unsafe extern "C" fn stream_element(vmctx: *mut VMContext, thunk: *mut u8) -> *mut u8 {
    // Read captures before any allocation (the thunk may move on GC).
    let id = *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET) as *const u64);
    let idx = *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 8) as *const u64) as usize;

    // ToCore conversion is (potentially) user code: contain panics.
    let converted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        current_machine()
            .and_then(|ms| ms.parked_stream_get(StreamId(id), |ps| ps.source.get(idx, &ps.table)))
    }));

    let value = match converted {
        Ok(Some(Some(Ok(v)))) => v,
        Ok(Some(Some(Err(e)))) => {
            let msg = format!("stream element conversion failed: {e}");
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Ok(Some(None)) => {
            let msg = b"effect result stream: element index out of bounds";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Ok(None) => {
            let msg = b"effect result stream: registry entry missing (stale continuation?)";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Err(panic) => {
            let what = panic
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            let msg = format!("effect result stream: element conversion panicked: {what}");
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    // Materialize with one GC-and-retry (value is GC-inert Rust data).
    match crate::heap_bridge::value_to_heap(&value, &mut *vmctx) {
        Ok(p) => p,
        Err(crate::heap_bridge::BridgeError::NurseryExhausted) => {
            gc_trigger(vmctx);
            match crate::heap_bridge::value_to_heap(&value, &mut *vmctx) {
                Ok(p) => p,
                Err(_) => runtime_oom(),
            }
        }
        Err(e) => {
            let msg = format!("stream element materialization failed: {e}");
            push_diagnostic(msg.clone());
            runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64)
        }
    }
}
