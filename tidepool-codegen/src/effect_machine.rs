use crate::context::VMContext;
use crate::heap_bridge;
use crate::yield_type::{Yield, YieldError};
use tidepool_heap::layout;

/// Constructor tags for the freer-simple Eff type.
///
/// These identify which DataCon a heap-allocated constructor represents,
/// allowing the effect machine to distinguish Val (pure result) from
/// E (effect request) and destructure Union wrappers and Leaf/Node continuations.
#[derive(Debug, Clone, Copy)]
pub struct ConTags {
    /// Con_tag for the Val constructor (pure result).
    pub val: u64,
    /// Con_tag for the E constructor (effect request).
    pub e: u64,
    /// Con_tag for the Union constructor (effect type wrapper).
    pub union: u64,
    /// Con_tag for the Leaf constructor (leaf continuation).
    pub leaf: u64,
    /// Con_tag for the Node constructor (composed continuation).
    pub node: u64,
}

impl ConTags {
    /// Resolve freer-simple constructor tags from a DataConTable.
    pub fn from_table(table: &tidepool_repr::DataConTable) -> Option<Self> {
        Some(ConTags {
            val: table.get_by_name("Val")?.0,
            e: table.get_by_name("E")?.0,
            union: table.get_by_name("Union")?.0,
            leaf: table.get_by_name("Leaf")?.0,
            node: table.get_by_name("Node")?.0,
        })
    }
}

/// Compiled effect machine — drives JIT-compiled freer-simple effect stacks.
///
/// The step/resume protocol:
/// 1. step() calls the compiled function, parses the result:
///    - Con with Val con_tag → Yield::Done(value)
///    - Con with E con_tag → Yield::Request(tag, request, continuation)
/// 2. resume(continuation, response) applies the continuation tree to the response
///    and parses the resulting heap object.
pub struct CompiledEffectMachine {
    func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8,
    vmctx: VMContext,
    tags: ConTags,
}

// SAFETY: All fields are raw pointers or function pointers, which are Send.
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
            tags,
        }
    }

    /// Access the VMContext (e.g., to update nursery pointers after GC).
    pub fn vmctx_mut(&mut self) -> &mut VMContext {
        &mut self.vmctx
    }

    /// Execute the compiled function and parse the result.
    pub fn step(&mut self) -> Yield {
        let mut result: *mut u8 = unsafe { (self.func_ptr)(&mut self.vmctx) };
        // TCO: resolve pending tail calls
        unsafe { self.resolve_tail_calls(&mut result); }
        self.parse_result(result)
    }

    /// Resume after handling an effect by applying the continuation to the response.
    ///
    /// # Safety
    ///
    /// `continuation` and `response` must be valid heap pointers from the nursery.
    pub unsafe fn resume(&mut self, continuation: *mut u8, response: *mut u8) -> Yield {
        let result = self.apply_cont_heap(continuation, response);
        self.parse_result(result)
    }

    /// Parse a heap-allocated Eff result into a Yield.
    fn parse_result(&mut self, result: *mut u8) -> Yield {
        // Check for runtime error FIRST (before null check), because runtime_error
        // now returns a "poison" non-null Lit object to prevent segfaults in JIT code.
        if let Some(err) = crate::host_fns::take_runtime_error() {
            return Yield::Error(YieldError::from(err));
        }
        if result.is_null() {
            return Yield::Error(YieldError::NullPointer);
        }

        // Force result if it's a thunk (lazy Con field from parent)
        let result = self.force_ptr(result);
        if result.is_null() {
            return Yield::Error(YieldError::NullPointer);
        }

        let tag = unsafe { *result };
        if tag != layout::TAG_CON {
            return Yield::Error(YieldError::UnexpectedTag(tag));
        }

        let con_tag = unsafe { *(result.add(layout::CON_TAG_OFFSET) as *const u64) };

        if con_tag == self.tags.val {
            // Val(value) — extract value from fields[0]
            let num_fields = unsafe { *(result.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16) };
            if num_fields < 1 {
                return Yield::Error(YieldError::BadValFields(num_fields));
            }
            let value = unsafe { *(result.add(layout::CON_FIELDS_OFFSET) as *const *mut u8) };
            // Force value field — it may be a thunk
            let value = self.force_ptr(value);
            Yield::Done(value)
        } else if con_tag == self.tags.e {
            // E(union, continuation) — extract Union and k
            let num_fields = unsafe { *(result.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16) };
            if num_fields != 2 {
                return Yield::Error(YieldError::BadEFields(num_fields));
            }
            let mut union_ptr =
                unsafe { *(result.add(layout::CON_FIELDS_OFFSET) as *const *mut u8) };
            let mut continuation =
                unsafe { *(result.add(layout::CON_FIELDS_OFFSET + 8) as *const *mut u8) };

            // Force all field pointers — they may be thunks from lazy Con fields
            union_ptr = self.force_ptr(union_ptr);
            if union_ptr.is_null() {
                return Yield::Error(YieldError::NullPointer);
            }
            continuation = self.force_ptr(continuation);
            if continuation.is_null() {
                return Yield::Error(YieldError::NullPointer);
            }

            let union_tag = unsafe { *union_ptr };
            if union_tag != layout::TAG_CON {
                return Yield::Error(YieldError::UnexpectedTag(union_tag));
            }

            let union_num_fields =
                unsafe { *(union_ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16) };
            if union_num_fields != 2 {
                return Yield::Error(YieldError::BadUnionFields(union_num_fields));
            }

            let tag_ptr = unsafe { *(union_ptr.add(layout::CON_FIELDS_OFFSET) as *const *mut u8) };
            let tag_ptr = self.force_ptr(tag_ptr);
            if tag_ptr.is_null() {
                return Yield::Error(YieldError::NullPointer);
            }
            // Read the actual tag value from the Lit HeapObject (offset 16 = LIT_VALUE_OFFSET)
            let tag_ptr_tag = unsafe { *tag_ptr };
            let effect_tag = unsafe { *(tag_ptr.add(layout::LIT_VALUE_OFFSET) as *const u64) };
            let mut request =
                unsafe { *(union_ptr.add(layout::CON_FIELDS_OFFSET + 8) as *const *mut u8) };
            request = self.force_ptr(request);

            if std::env::var("TIDEPOOL_TRACE_EFFECTS").is_ok() {
                eprintln!(
                    "[effect_machine] effect_tag={} tag_ptr_tag={} union_con_tag={} request_tag={}",
                    effect_tag,
                    tag_ptr_tag,
                    unsafe { *(union_ptr.add(layout::CON_TAG_OFFSET) as *const u64) },
                    if request.is_null() {
                        255
                    } else {
                        unsafe { *request }
                    }
                );
            }

            Yield::Request {
                tag: effect_tag,
                request,
                continuation,
            }
        } else {
            Yield::Error(YieldError::UnexpectedConTag(con_tag))
        }
    }

    /// Force a heap pointer if it's a thunk, returning the WHNF result.
    /// Loops to handle chains (thunk returning thunk).
    fn force_ptr(&mut self, ptr: *mut u8) -> *mut u8 {
        let mut current = ptr;
        loop {
            if current.is_null() {
                return current;
            }
            let tag = unsafe { *current };
            if tag == layout::TAG_THUNK {
                let vmctx = &mut self.vmctx as *mut VMContext;
                current = crate::host_fns::heap_force(vmctx, current);
            } else {
                return current;
            }
        }
    }

    /// Apply a Leaf/Node continuation tree to a value, yielding a new Eff result.
    ///
    /// Mirrors the interpreter's `apply_cont` on raw heap pointers:
    /// - Leaf(f): call f(arg) via call_closure
    /// - Node(k1, k2): apply k1(arg), if Val(y) → k2(y), if E(union, k') → E(union, Node(k', k2))
    /// - Closure: direct call_closure (degenerate continuation fallback)
    ///
    /// # Safety
    ///
    /// `k` and `arg` must be valid heap pointers.
    unsafe fn apply_cont_heap(&mut self, k: *mut u8, arg: *mut u8) -> *mut u8 {
        if k.is_null() {
            return std::ptr::null_mut();
        }

        // Force k and arg in case they are thunks (lazy Con fields)
        let k = self.force_ptr(k);
        if k.is_null() {
            return std::ptr::null_mut();
        }
        let arg = self.force_ptr(arg);

        let tag = *k;
        match tag {
            t if t == layout::TAG_CON => {
                let con_tag = *(k.add(layout::CON_TAG_OFFSET) as *const u64);

                if con_tag == self.tags.leaf {
                    // Leaf(f) — extract closure f at field[0], call f(arg)
                    let f = self.force_ptr(*(k.add(layout::CON_FIELDS_OFFSET) as *const *mut u8));
                    self.call_closure(f, arg)
                } else if con_tag == self.tags.node {
                    // Node(k1, k2) — apply k1 to arg, then compose with k2
                    let k1 = self.force_ptr(*(k.add(layout::CON_FIELDS_OFFSET) as *const *mut u8));
                    let k2 =
                        self.force_ptr(*(k.add(layout::CON_FIELDS_OFFSET + 8) as *const *mut u8));

                    let result = self.apply_cont_heap(k1, arg);
                    if result.is_null() {
                        return std::ptr::null_mut();
                    }

                    // Force result in case it's a thunk
                    let result = self.force_ptr(result);
                    if result.is_null() {
                        return std::ptr::null_mut();
                    }

                    // Check if result is Val or E
                    let result_tag = *result;
                    if result_tag != layout::TAG_CON {
                        return std::ptr::null_mut();
                    }

                    let result_con_tag = *(result.add(layout::CON_TAG_OFFSET) as *const u64);

                    if result_con_tag == self.tags.val {
                        // Val(y) — extract y, apply k2(y)
                        let y = self
                            .force_ptr(*(result.add(layout::CON_FIELDS_OFFSET) as *const *mut u8));
                        self.apply_cont_heap(k2, y)
                    } else if result_con_tag == self.tags.e {
                        // E(union, k') — compose: E(union, Node(k', k2))
                        let union_val = self
                            .force_ptr(*(result.add(layout::CON_FIELDS_OFFSET) as *const *mut u8));
                        let k_prime = self.force_ptr(
                            *(result.add(layout::CON_FIELDS_OFFSET + 8) as *const *mut u8),
                        );

                        // Allocate Node(k', k2)
                        let new_node = self.alloc_con(self.tags.node, &[k_prime, k2]);
                        if new_node.is_null() {
                            return std::ptr::null_mut();
                        }
                        // Allocate E(union, new_node)
                        self.alloc_con(self.tags.e, &[union_val, new_node])
                    } else {
                        std::ptr::null_mut()
                    }
                } else {
                    // Unknown Con tag in continuation position — error
                    std::ptr::null_mut()
                }
            }
            t if t == layout::TAG_CLOSURE => {
                // Raw closure (degenerate continuation fallback)
                self.call_closure(k, arg)
            }
            t if t == layout::TAG_THUNK => {
                // Thunk in continuation position — already forced above, shouldn't happen
                std::ptr::null_mut()
            }
            _ => std::ptr::null_mut(),
        }
    }

    /// Call a compiled closure: read code_ptr from closure[8], invoke it.
    ///
    /// # Safety
    ///
    /// `closure` must point to a valid Closure HeapObject.
    unsafe fn call_closure(&mut self, closure: *mut u8, arg: *mut u8) -> *mut u8 {
        let code_ptr = *(closure.add(layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);

        let trace = crate::debug::trace_level();
        if trace >= crate::debug::TraceLevel::Calls {
            let name = crate::debug::lookup_lambda(code_ptr)
                .unwrap_or_else(|| format!("0x{:x}", code_ptr));
            eprintln!(
                "[trace] call_closure {} closure={:?} arg={}",
                name,
                closure,
                crate::debug::heap_describe(arg),
            );
        }
        if trace >= crate::debug::TraceLevel::Heap {
            if let Err(e) = crate::debug::heap_validate_deep(closure) {
                eprintln!("[trace] INVALID closure: {}", e);
                eprintln!("[trace]   {}", crate::debug::heap_describe(closure));
                return std::ptr::null_mut();
            }
            if let Err(e) = crate::debug::heap_validate(arg) {
                eprintln!("[trace] INVALID arg: {}", e);
                return std::ptr::null_mut();
            }
            // Dump captures
            let num_captured = *(closure.add(layout::CLOSURE_NUM_CAPTURED_OFFSET) as *const u16);
            for i in 0..num_captured as usize {
                let cap =
                    *(closure.add(layout::CLOSURE_CAPTURED_OFFSET + 8 * i) as *const *const u8);
                if cap.is_null() {
                    eprintln!("[trace]   capture[{}] = NULL", i);
                } else {
                    eprintln!(
                        "[trace]   capture[{}] = {}",
                        i,
                        crate::debug::heap_describe(cap)
                    );
                }
            }
        }

        let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(code_ptr);
        let mut result = func(&mut self.vmctx, closure, arg);
        // TCO: resolve pending tail calls
        unsafe {
            self.resolve_tail_calls(&mut result);
        }

        if trace >= crate::debug::TraceLevel::Calls {
            let name = crate::debug::lookup_lambda(code_ptr)
                .unwrap_or_else(|| format!("0x{:x}", code_ptr));
            if result.is_null() {
                eprintln!("[trace] {} returned NULL", name);
            } else {
                eprintln!(
                    "[trace] {} returned {}",
                    name,
                    crate::debug::heap_describe(result)
                );
            }
        }

        result
    }

    /// Resolve pending tail calls stored in VMContext by the JIT.
    ///
    /// # Safety
    /// VMContext must have valid tail_callee/tail_arg if non-null.
    unsafe fn resolve_tail_calls(&mut self, result: &mut *mut u8) {
        while result.is_null() && !self.vmctx.tail_callee.is_null() {
            let callee = self.vmctx.tail_callee;
            let arg = self.vmctx.tail_arg;
            self.vmctx.tail_callee = std::ptr::null_mut();
            self.vmctx.tail_arg = std::ptr::null_mut();
            crate::host_fns::reset_call_depth();
            let code_ptr = *(callee.add(layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);
            let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
                std::mem::transmute(code_ptr);
            *result = func(&mut self.vmctx, callee, arg);
        }
    }

    /// Allocate a Con HeapObject on the nursery with the given tag and fields.
    unsafe fn alloc_con(&mut self, con_tag: u64, fields: &[*mut u8]) -> *mut u8 {
        let size = 24 + 8 * fields.len();
        let ptr = heap_bridge::bump_alloc_from_vmctx(&mut self.vmctx, size);
        if ptr.is_null() {
            return std::ptr::null_mut();
        }
        layout::write_header(ptr, layout::TAG_CON, size as u16);
        *(ptr.add(layout::CON_TAG_OFFSET) as *mut u64) = con_tag;
        *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = fields.len() as u16;
        for (i, &fp) in fields.iter().enumerate() {
            *(ptr.add(layout::CON_FIELDS_OFFSET + 8 * i) as *mut *mut u8) = fp;
        }
        ptr
    }
}
