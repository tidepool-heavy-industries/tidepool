use crate::context::VMContext;

/// Owned bump-allocator nursery for JIT-compiled code.
///
/// Provides the backing memory that VMContext's alloc_ptr/alloc_limit point into.
/// No GC — panics on exhaustion.
pub struct Nursery {
    /// `Vec<u64>`, not `Vec<u8>` (L8, repo-review-2026-07-06/01-gc-memory-
    /// safety.md): heap objects stored here are read/written assuming
    /// 8-byte alignment, but a `Vec<u8>`'s OWN element alignment is 1 — any
    /// 8-byte alignment it happens to have is an accident of the global
    /// allocator (glibc malloc always returns suitably-aligned memory for
    /// non-tiny sizes) rather than anything the type system guarantees.
    /// `Vec<u64>` makes the guarantee structural: its allocation is ALWAYS
    /// 8-byte aligned regardless of allocator, holding even under an
    /// allocator swap. Exposed to callers as raw `*const/*mut u8`; `size()`
    /// is always a multiple of 8 (rounded up from the requested byte count).
    buffer: Vec<u64>,
}

impl Nursery {
    /// Create a nursery with the given size in bytes (rounded up to the
    /// next multiple of 8 if not already one).
    pub fn new(size: usize) -> Self {
        Self {
            buffer: vec![0u64; size.div_ceil(8)],
        }
    }

    /// Get the start address of the nursery buffer.
    pub fn start(&self) -> *const u8 {
        self.buffer.as_ptr() as *const u8
    }

    /// Get the size of the nursery buffer in bytes (a multiple of 8; see
    /// the `buffer` field doc for why).
    pub fn size(&self) -> usize {
        self.buffer.len() * 8
    }

    /// Create a VMContext pointing into this nursery.
    ///
    /// The returned VMContext is valid as long as this Nursery is alive
    /// and not moved.
    pub fn make_vmctx(&mut self, gc_trigger: unsafe extern "C" fn(*mut VMContext)) -> VMContext {
        let start = self.buffer.as_mut_ptr() as *mut u8;
        // SAFETY: start points to a Vec<u64> buffer of `self.size()` bytes;
        // adding that length stays within the allocation.
        let end = unsafe { start.add(self.size()) };
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
        assert_eq!(nursery.size(), size);
        assert!(nursery.buffer.iter().all(|&w| w == 0));
    }

    /// A byte count that isn't already a multiple of 8 rounds UP, never
    /// down — the caller must always get at least what it asked for.
    #[test]
    fn test_nursery_new_rounds_up_to_word_multiple() {
        let nursery = Nursery::new(1023);
        assert_eq!(nursery.size(), 1024);
        let nursery2 = Nursery::new(1025);
        assert_eq!(nursery2.size(), 1032);
    }

    #[test]
    fn test_make_vmctx() {
        let size = 1024;
        let mut nursery = Nursery::new(size);
        let vmctx = nursery.make_vmctx(dummy_gc_trigger);

        assert_eq!(vmctx.alloc_ptr, nursery.buffer.as_mut_ptr() as *mut u8);
        // SAFETY: nursery.buffer backs `size()` bytes; adding that stays within bounds.
        assert_eq!(vmctx.alloc_limit, unsafe {
            (nursery.buffer.as_ptr() as *const u8).add(size)
        });
        assert_eq!(
            vmctx.gc_trigger as usize,
            dummy_gc_trigger as *const () as usize
        );
    }

    /// L8: alignment is now a STRUCTURAL guarantee of `Vec<u64>` backing,
    /// not an accident of glibc malloc's behavior for `Vec<u8>` — this
    /// holds regardless of the global allocator in use.
    #[test]
    fn test_vmctx_alignment() {
        let size = 1024;
        let mut nursery = Nursery::new(size);
        let vmctx = nursery.make_vmctx(dummy_gc_trigger);
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
