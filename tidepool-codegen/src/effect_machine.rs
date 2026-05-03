use crate::context::VMContext;
use crate::heap_bridge;
use crate::layout;
use crate::yield_type::{Yield, YieldError};
use tidepool_heap::layout as heap_layout;

/// The five freer-simple continuation constructors that the effect machine must resolve.
#[derive(Debug, Clone, Copy)]
pub enum EffContKind {
    Val,
    E,
    Union,
    Leaf,
    Node,
}

impl EffContKind {
    /// The constructor name as it appears in the DataConTable.
    pub fn name(self) -> &'static str {
        match self {
            EffContKind::Val => "Val",
            EffContKind::E => "E",
            EffContKind::Union => "Union",
            EffContKind::Leaf => "Leaf",
            EffContKind::Node => "Node",
        }
    }

    /// All variants in registration order.
    pub const ALL: [EffContKind; 5] = [
        EffContKind::Val,
        EffContKind::E,
        EffContKind::Union,
        EffContKind::Leaf,
        EffContKind::Node,
    ];
}

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

impl TryFrom<&tidepool_repr::DataConTable> for ConTags {
    type Error = EffContKind;

    fn try_from(table: &tidepool_repr::DataConTable) -> Result<Self, Self::Error> {
        let resolve = |kind: EffContKind| -> Result<u64, EffContKind> {
            table.get_by_name(kind.name()).map(|t| t.0).ok_or(kind)
        };
        Ok(ConTags {
            val: resolve(EffContKind::Val)?,
            e: resolve(EffContKind::E)?,
            union: resolve(EffContKind::Union)?,
            leaf: resolve(EffContKind::Leaf)?,
            node: resolve(EffContKind::Node)?,
        })
    }
}

impl ConTags {
    pub fn from_table(table: &tidepool_repr::DataConTable) -> Result<Self, EffContKind> {
        Self::try_from(table)
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
    /// Read the constructor tag from a Con heap object.
    ///
    /// # Safety
    /// `ptr` must point to a valid Con heap object (tag byte == TAG_CON).
    unsafe fn read_con_tag(ptr: *const u8) -> u64 {
        *(ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64)
    }

    /// Read the number of fields from a Con heap object.
    ///
    /// # Safety
    /// `ptr` must point to a valid Con heap object.
    unsafe fn read_con_num_fields(ptr: *const u8) -> u16 {
        *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
    }

    /// Read a field pointer from a Con heap object by index.
    ///
    /// # Safety
    /// `ptr` must point to a valid Con heap object with at least `index + 1` fields.
    unsafe fn read_con_field(ptr: *const u8, index: usize) -> *mut u8 {
        *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * index) as *const *mut u8)
    }

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
        // SAFETY: func_ptr is a finalized JIT function pointer. vmctx is valid and
        // owned by this machine. The function returns a heap pointer to an Eff value.
        let mut result: *mut u8 = unsafe { (self.func_ptr)(&mut self.vmctx) };
        // SAFETY: resolve_tail_calls reads/writes vmctx.tail_callee/tail_arg which
        // are valid heap pointers set by JIT tail-call sites.
        unsafe {
            self.resolve_tail_calls(&mut result);
        }
        self.parse_result(result)
    }

    /// Resume after handling an effect by applying the continuation to the response.
    ///
    /// # Safety
    ///
    /// `continuation` and `response` must be valid heap pointers from the nursery.
    pub unsafe fn resume(&mut self, continuation: *mut u8, response: *mut u8) -> Yield {
        // SAFETY: Caller guarantees continuation and response are valid nursery heap pointers.
        let mut result = self.apply_cont_heap(continuation, response);
        self.resolve_tail_calls(&mut result);
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

        // SAFETY: result is non-null (checked above) and points to a valid heap object.
        // All field reads below use known layout offsets from tidepool_heap::layout.
        let tag = unsafe { *result };
        if tag != layout::TAG_CON {
            return Yield::Error(YieldError::UnexpectedTag(tag));
        }

        let con_tag = unsafe { Self::read_con_tag(result) };

        if con_tag == self.tags.val {
            // Val(value) — extract value from fields[0]
            let num_fields = unsafe { Self::read_con_num_fields(result) };
            if num_fields < 1 {
                return Yield::Error(YieldError::BadValFields(num_fields));
            }
            let value = unsafe { Self::read_con_field(result, 0) };
            // Force value field — it may be a thunk
            let value = self.force_ptr(value);
            Yield::Done(value)
        } else if con_tag == self.tags.e {
            // E(union, continuation) — extract Union and k
            let num_fields = unsafe { Self::read_con_num_fields(result) };
            if num_fields != 2 {
                return Yield::Error(YieldError::BadEFields(num_fields));
            }
            let mut union_ptr = unsafe { Self::read_con_field(result, 0) };
            let mut continuation = unsafe { Self::read_con_field(result, 1) };

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

            let union_num_fields = unsafe { Self::read_con_num_fields(union_ptr) };
            if union_num_fields != 2 {
                return Yield::Error(YieldError::BadUnionFields(union_num_fields));
            }

            let tag_ptr = unsafe { Self::read_con_field(union_ptr, 0) };
            let tag_ptr = self.force_ptr(tag_ptr);
            if tag_ptr.is_null() {
                return Yield::Error(YieldError::NullPointer);
            }
            // Read the actual effect tag value. The Union's first field is the
            // position index (Word#). After GHC optimization in single-module
            // compilation, this is an unboxed Lit(Word, N). In cross-module
            // compilation, the boxing may survive as Con(W#, [Lit(Word, N)]).
            // Handle both layouts to avoid reading garbage.
            let tag_ptr_tag = unsafe { *tag_ptr };
            let effect_tag = if tag_ptr_tag == layout::TAG_LIT {
                // Unboxed: read value directly from Lit.
                unsafe { *(tag_ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const u64) }
            } else if tag_ptr_tag == layout::TAG_CON {
                // Boxed (W# n): peel the box — read field[0] which is the Lit.
                let inner = unsafe { Self::read_con_field(tag_ptr, 0) };
                let inner = self.force_ptr(inner);
                if inner.is_null() {
                    return Yield::Error(YieldError::NullPointer);
                }
                unsafe { *(inner.add(layout::LIT_VALUE_OFFSET as usize) as *const u64) }
            } else {
                // Unexpected heap tag for Union position.
                return Yield::Error(YieldError::UnexpectedTag(tag_ptr_tag));
            };
            let mut request = unsafe { Self::read_con_field(union_ptr, 1) };
            request = self.force_ptr(request);

            if std::env::var("TIDEPOOL_TRACE_EFFECTS").is_ok() {
                eprintln!(
                    "[effect_machine] effect_tag={} tag_ptr_tag={} union_con_tag={} request_tag={}",
                    effect_tag,
                    tag_ptr_tag,
                    unsafe { Self::read_con_tag(union_ptr) },
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
            // SAFETY: current is non-null (checked above) and points to a valid heap object.
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
    /// Uses an iterative work-stack instead of recursion. Heap pointers held across
    /// `call_closure` (which can trigger GC) are stored in a `Vec` on the Rust heap
    /// and registered as GC roots so the collector can update them in-place.
    ///
    /// # Safety
    ///
    /// `k` and `arg` must be valid heap pointers.
    unsafe fn apply_cont_heap(&mut self, k: *mut u8, arg: *mut u8) -> *mut u8 {
        // SAFETY: k and arg are valid heap pointers (or null, handled below).
        // All field reads use known layout offsets.
        if k.is_null() {
            return std::ptr::null_mut();
        }

        let mut k = self.force_ptr(k);
        if k.is_null() {
            return std::ptr::null_mut();
        }
        let mut arg = self.force_ptr(arg);

        // Stack of pending k2 continuations from Node decomposition.
        // Lives on the Rust heap, not the GC nursery. Entries are heap pointers
        // that must be registered as GC roots before any call_closure.
        let mut k2_stack: Vec<*mut u8> = Vec::new();

        loop {
            if k.is_null() {
                return std::ptr::null_mut();
            }

            let tag = *k;
            let result = match tag {
                t if t == layout::TAG_CON => {
                    let con_tag = Self::read_con_tag(k);

                    if con_tag == self.tags.leaf {
                        // Leaf(f): call f(arg) — terminal for this continuation
                        let f = self.force_ptr(Self::read_con_field(k, 0));
                        // Register k2_stack entries as GC roots before call_closure,
                        // which runs JIT code that can trigger GC.
                        for slot in k2_stack.iter_mut() {
                            crate::host_fns::register_rust_root(slot as *mut *mut u8);
                        }
                        let res = self.call_closure(f, arg);
                        crate::host_fns::clear_rust_roots();
                        res
                    } else if con_tag == self.tags.node {
                        // Node(k1, k2): push k2 for later, loop on k1
                        let k1 = self.force_ptr(Self::read_con_field(k, 0));
                        let k2 = self.force_ptr(Self::read_con_field(k, 1));
                        k2_stack.push(k2);
                        k = k1;
                        continue;
                    } else {
                        crate::host_fns::push_diagnostic(format!(
                            "apply_cont_heap: unexpected continuation con_tag {} (expected Leaf or Node)",
                            con_tag
                        ));
                        return std::ptr::null_mut();
                    }
                }
                t if t == layout::TAG_CLOSURE => {
                    // Raw closure (degenerate continuation fallback)
                    for slot in k2_stack.iter_mut() {
                        crate::host_fns::register_rust_root(slot as *mut *mut u8);
                    }
                    let res = self.call_closure(k, arg);
                    crate::host_fns::clear_rust_roots();
                    res
                }
                _ => {
                    crate::host_fns::push_diagnostic(format!(
                        "apply_cont_heap: unexpected heap tag {} in continuation position",
                        tag
                    ));
                    return std::ptr::null_mut();
                }
            };

            // We have a result from call_closure. Compose with pending k2s.
            if result.is_null() {
                return std::ptr::null_mut();
            }
            let result = self.force_ptr(result);
            if result.is_null() {
                return std::ptr::null_mut();
            }

            let result_tag = *result;
            if result_tag != layout::TAG_CON {
                crate::host_fns::push_diagnostic(format!(
                    "apply_cont_heap: result has unexpected tag {} (expected TAG_CON)",
                    result_tag
                ));
                return std::ptr::null_mut();
            }

            let result_con_tag = Self::read_con_tag(result);

            if result_con_tag == self.tags.val {
                // Val(y): if k2_stack is empty, we're done; otherwise apply next k2
                let y = self.force_ptr(Self::read_con_field(result, 0));
                if let Some(k2) = k2_stack.pop() {
                    k = k2;
                    arg = y;
                    continue;
                } else {
                    return result;
                }
            } else if result_con_tag == self.tags.e {
                // E(union, k'): compose ALL remaining k2s into k'
                let union_val = self.force_ptr(Self::read_con_field(result, 0));
                let mut k_prime = self.force_ptr(Self::read_con_field(result, 1));

                while let Some(k2) = k2_stack.pop() {
                    k_prime = self.alloc_con(self.tags.node, &[k_prime, k2]);
                    if k_prime.is_null() {
                        return std::ptr::null_mut();
                    }
                }
                return self.alloc_con(self.tags.e, &[union_val, k_prime]);
            } else {
                crate::host_fns::push_diagnostic(format!(
                    "apply_cont_heap: result con_tag {} is neither Val nor E",
                    result_con_tag
                ));
                return std::ptr::null_mut();
            }
        }
    }

    /// Call a compiled closure: read code_ptr from closure[8], invoke it.
    ///
    /// # Safety
    ///
    /// `closure` must point to a valid Closure HeapObject.
    unsafe fn call_closure(&mut self, closure: *mut u8, arg: *mut u8) -> *mut u8 {
        // SAFETY: closure is a valid Closure heap object. Reading code_ptr at the known offset.
        let code_ptr = *(closure.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);

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
            let num_captured =
                *(closure.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16);
            for i in 0..num_captured as usize {
                let cap = *(closure.add(layout::CLOSURE_CAPTURED_OFFSET as usize + 8 * i)
                    as *const *const u8);
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

        // SAFETY: code_ptr was set during JIT compilation and points to a finalized
        // Cranelift function with the closure calling convention (vmctx, self, arg) -> result.
        let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(code_ptr);
        let mut result = func(&mut self.vmctx, closure, arg);
        // SAFETY: After a closure call, pending tail calls may be stored in vmctx.
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
        // SAFETY: tail_callee and tail_arg are valid heap pointers set by JIT tail-call
        // sites. Code pointers in closures point to finalized JIT functions.
        while result.is_null() && !self.vmctx.tail_callee.is_null() {
            // External cancellation safepoint — an infinite tail-recursive
            // loop must be interruptible. See `host_fns::trampoline_resolve`
            // for the rationale.
            if crate::host_fns::check_cancel_and_set_error() {
                self.vmctx.tail_callee = std::ptr::null_mut();
                self.vmctx.tail_arg = std::ptr::null_mut();
                *result = crate::host_fns::error_poison_ptr();
                return;
            }

            let callee = self.vmctx.tail_callee;
            let arg = self.vmctx.tail_arg;
            self.vmctx.tail_callee = std::ptr::null_mut();
            self.vmctx.tail_arg = std::ptr::null_mut();
            crate::host_fns::reset_call_depth();
            let code_ptr = *(callee.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
            let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
                std::mem::transmute(code_ptr);
            *result = func(&mut self.vmctx, callee, arg);
        }
    }

    /// Allocate a Con HeapObject on the nursery with the given tag and fields.
    unsafe fn alloc_con(&mut self, con_tag: u64, fields: &[*mut u8]) -> *mut u8 {
        // SAFETY: Bump-allocating from vmctx nursery. Writing Con header, tag,
        // num_fields, and field pointers at known layout offsets within the allocation.
        let size = 24 + 8 * fields.len();
        let ptr = heap_bridge::bump_alloc_from_vmctx(&mut self.vmctx, size);
        if ptr.is_null() {
            return std::ptr::null_mut();
        }
        heap_layout::write_header(ptr, layout::TAG_CON, size as u16);
        *(ptr.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = con_tag;
        *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = fields.len() as u16;
        for (i, &fp) in fields.iter().enumerate() {
            *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * i) as *mut *mut u8) = fp;
        }
        ptr
    }
}
