use crate::context::VMContext;

/// Owned bump-allocator nursery for JIT-compiled code.
///
/// Provides the backing memory that VMContext's alloc_ptr/alloc_limit point into.
/// No GC — panics on exhaustion.
pub struct Nursery {
    buffer: Vec<u8>,
}

impl Nursery {
    /// Create a nursery with the given size in bytes.
    pub fn new(size: usize) -> Self {
        Self {
            buffer: vec![0u8; size],
        }
    }

    /// Create a VMContext pointing into this nursery.
    ///
    /// The returned VMContext is valid as long as this Nursery is alive
    /// and not moved.
    pub fn make_vmctx(&mut self, gc_trigger: extern "C" fn(*mut VMContext)) -> VMContext {
        let start = self.buffer.as_mut_ptr();
        let end = unsafe { start.add(self.buffer.len()) };
        VMContext::new(start, end as *const u8, gc_trigger)
    }
}
