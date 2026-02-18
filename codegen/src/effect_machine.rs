use crate::context::VMContext;
use crate::yield_type::{Yield, YieldError};

/// Constructor tags for the freer-simple Eff type.
///
/// These identify which DataCon a heap-allocated constructor represents,
/// allowing the effect machine to distinguish Val (pure result) from
/// E (effect request) and destructure Union wrappers.
#[derive(Debug, Clone, Copy)]
pub struct ConTags {
    /// Con_tag for the Val constructor (pure result).
    pub val: u64,
    /// Con_tag for the E constructor (effect request).
    pub e: u64,
    /// Con_tag for the Union constructor (effect type wrapper).
    pub union: u64,
}

/// Compiled effect machine — drives JIT-compiled freer-simple effect stacks.
///
/// The step/resume protocol:
/// 1. step() calls the compiled function, reads result tag:
///    - Con with Val con_tag → Yield::Done(value)
///    - Con with E con_tag → Yield::Request(tag, request, continuation)
/// 2. resume(result) constructs App(continuation, result) as a new heap object,
///    sets it as the current expression, ready for next step()
///
/// GC runs only between steps (clean collection points).
pub struct CompiledEffectMachine {
    /// Compiled function pointer: fn(vmctx: *mut VMContext) -> *mut u8
    func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8,
    /// VM context with nursery pointers.
    vmctx: VMContext,
    /// Con_tag value for Val constructor.
    val_con_tag: u64,
    /// Con_tag value for E constructor.
    e_con_tag: u64,
    /// Con_tag value for Union constructor.
    #[allow(dead_code)]
    union_con_tag: u64,
}

// SAFETY: All fields are raw pointers or function pointers, which are Send.
// The EffectMachine does not contain Rc, RefCell, or thread-local references.
unsafe impl Send for CompiledEffectMachine {}

impl CompiledEffectMachine {
    pub fn new(
        func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8,
        vmctx: VMContext,
        tags: ConTags,
    ) -> Self {
        Self {
            func_ptr,
            vmctx,
            val_con_tag: tags.val,
            e_con_tag: tags.e,
            union_con_tag: tags.union,
        }
    }

    /// Access the VMContext (e.g., to update nursery pointers after GC).
    pub fn vmctx_mut(&mut self) -> &mut VMContext {
        &mut self.vmctx
    }

    /// Resume after handling an effect. The caller provides the response value
    /// as a heap pointer, and a new compiled function that represents
    /// App(continuation, response).
    ///
    /// For v1, the caller is responsible for constructing the application
    /// and providing the next function to call. This will be wired up
    /// in the integration phase.
    pub fn set_next(&mut self, func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8) {
        self.func_ptr = func_ptr;
    }

    pub fn step(&mut self) -> Yield {
        // Call compiled function
        let result: *mut u8 = unsafe { (self.func_ptr)(&mut self.vmctx) };
        if result.is_null() {
            return Yield::Error(YieldError::NullPointer);
        }

        // Read tag byte at offset 0
        let tag = unsafe { *result };
        if tag != 2 {
            // TAG_CON
            return Yield::Error(YieldError::UnexpectedTag(tag));
        }

        // Read con_tag at offset 8
        let con_tag = unsafe { *(result.add(8) as *const u64) };

        if con_tag == self.val_con_tag {
            // Val(value) — extract value from fields[0]
            let num_fields = unsafe { *(result.add(16) as *const u16) };
            if num_fields < 1 {
                return Yield::Error(YieldError::BadValFields(num_fields));
            }
            let value = unsafe { *(result.add(24) as *const *mut u8) };
            Yield::Done(value)
        } else if con_tag == self.e_con_tag {
            // E(union, continuation) — extract Union and k
            let num_fields = unsafe { *(result.add(16) as *const u16) };
            if num_fields != 2 {
                return Yield::Error(YieldError::BadEFields(num_fields));
            }
            let union_ptr = unsafe { *(result.add(24) as *const *mut u8) };
            let continuation = unsafe { *(result.add(32) as *const *mut u8) };

            // Destructure Union(tag#, request)
            if union_ptr.is_null() {
                return Yield::Error(YieldError::NullPointer);
            }
            // First field of Union: tag (Word#) — stored as a Lit HeapObject or raw value
            let union_num_fields = unsafe { *(union_ptr.add(16) as *const u16) };
            if union_num_fields != 2 {
                return Yield::Error(YieldError::BadUnionFields(union_num_fields));
            }

            let tag_ptr = unsafe { *(union_ptr.add(24) as *const *mut u8) };
            if tag_ptr.is_null() {
                return Yield::Error(YieldError::NullPointer);
            }
            // Read the actual tag value from the Lit HeapObject (offset 16)
            let effect_tag = unsafe { *(tag_ptr.add(16) as *const u64) };
            let request = unsafe { *(union_ptr.add(32) as *const *mut u8) };

            Yield::Request {
                tag: effect_tag,
                request,
                continuation,
            }
        } else {
            Yield::Error(YieldError::UnexpectedConTag(con_tag))
        }
    }
}
