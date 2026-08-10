//! Eager, stack-safe materialization of list-shaped effect responses.
//!
//! A long list must never exist as a recursive structure on the host stack:
//! recursive `value_to_heap` (or a recursive `Value` Drop) costs ~3 frames
//! per cell and overflows the eval thread, with the fault landing outside
//! signal protection — a silent thread death. So the dispatch site hands
//! this module a FLAT `Vec` of pre-converted element `Value`s and the spine
//! is built back-to-front, iteratively, directly as heap cons cells.

use crate::context::VMContext;

use super::errors::{runtime_error_with_msg, runtime_oom};
use super::gc::{host_alloc_gc, register_rust_root, rust_roots_mark, truncate_rust_roots};

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
    tidepool_heap::layout::write_header(p, tidepool_heap::layout::TAG_CON, size as u32);
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
    let mark = rust_roots_mark(vmctx);
    let mut tail: *mut u8 = terminator;
    register_rust_root(vmctx, &mut tail as *mut *mut u8);

    // Build cells back-to-front so each cons links the already-built tail.
    let mut elem: *mut u8 = std::ptr::null_mut();
    register_rust_root(vmctx, &mut elem as *mut *mut u8);
    for v in items.iter().rev() {
        let converted = crate::heap_bridge::gc_retry(
            vmctx,
            |r: &Result<*mut u8, crate::heap_bridge::BridgeError>| {
                matches!(r, Err(crate::heap_bridge::BridgeError::NurseryExhausted))
            },
            || crate::heap_bridge::value_to_heap(v, &mut *vmctx),
        );
        elem = match converted {
            Ok(p) => p,
            Err(crate::heap_bridge::BridgeError::NurseryExhausted) => {
                truncate_rust_roots(vmctx, mark);
                return runtime_oom();
            }
            Err(_) => {
                truncate_rust_roots(vmctx, mark);
                let msg = b"effect result: element conversion failed";
                return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
            }
        };
        let size = tidepool_heap::layout::CON_FIELDS_OFFSET + 16;
        let cell = host_alloc_gc(vmctx, size);
        if cell.is_null() {
            truncate_rust_roots(vmctx, mark);
            return runtime_oom();
        }
        tidepool_heap::layout::write_header(cell, tidepool_heap::layout::TAG_CON, size as u32);
        *(cell.add(tidepool_heap::layout::CON_TAG_OFFSET) as *mut u64) = cons_tag;
        *(cell.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 2;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *mut *mut u8) = elem;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *mut *mut u8) = tail;
        tail = cell;
    }
    truncate_rust_roots(vmctx, mark);
    tail
}

/// Eagerly materialize a whole flattened list as a heap cons chain,
/// iteratively — no recursion over the spine. (A deep spine's recursive
/// `value_to_heap` or recursive `Value` Drop overflows the host stack; the
/// fault lands outside signal protection and silently kills the eval
/// thread.) This is the ONE list-response path — there is no lazy channel.
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
