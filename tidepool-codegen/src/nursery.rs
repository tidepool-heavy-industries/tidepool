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

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn dummy_gc_trigger(_vmctx: *mut VMContext) {}

    #[test]
    fn test_nursery_new() {
        let size = 1024;
        let nursery = Nursery::new(size);
        assert_eq!(nursery.buffer.len(), size);
        assert!(nursery.buffer.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_make_vmctx() {
        let size = 1024;
        let mut nursery = Nursery::new(size);
        let vmctx = nursery.make_vmctx(dummy_gc_trigger);

        assert_eq!(vmctx.alloc_ptr, nursery.buffer.as_mut_ptr());
        assert_eq!(vmctx.alloc_limit, unsafe { nursery.buffer.as_ptr().add(size) });
        assert_eq!(vmctx.gc_trigger as usize, dummy_gc_trigger as *const () as usize);
    }

    #[test]
    fn test_vmctx_alignment() {
        let size = 1024;
        let mut nursery = Nursery::new(size);
        let vmctx = nursery.make_vmctx(dummy_gc_trigger);

        // alloc_ptr should be 8-byte aligned (Vec's default alignment for u8 is likely 1, 
        // but it should be 8-byte aligned on most platforms for this size)
        assert_eq!(vmctx.alloc_ptr as usize % 8, 0);
    }

    #[test]
    fn test_multiple_vmctx() {
        let size = 1024;
        let mut nursery = Nursery::new(size);
        
        let vmctx1 = nursery.make_vmctx(dummy_gc_trigger);
        let vmctx2 = nursery.make_vmctx(dummy_gc_trigger);

        assert_eq!(vmctx1.alloc_ptr, vmctx2.alloc_ptr);
        assert_eq!(vmctx1.alloc_limit, vmctx2.alloc_limit);
    }
}
