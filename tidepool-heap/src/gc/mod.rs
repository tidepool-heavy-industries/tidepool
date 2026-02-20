pub mod compact;
pub mod trace;

use tidepool_eval::heap::{Heap, VecHeap};
use tidepool_eval::value::ThunkId;
use trace::ForwardingTable;

/// Run a full GC cycle: trace reachable thunks from roots, then compact
/// into a new VecHeap with only live thunks and rewritten ThunkId references.
pub fn collect(roots: &[ThunkId], heap: &dyn Heap) -> (VecHeap, ForwardingTable) {
    let table = trace::trace(roots, heap);
    let new_heap = compact::compact(&table, heap);
    (new_heap, table)
}
