use crate::context::VMContext;
use crate::heap_bridge;
use crate::layout;
use crate::machine_state::machine_state;
use crate::yield_type::{Yield, YieldError};
use tidepool_heap::layout as heap_layout;

// ---------------------------------------------------------------------------
// GC-safe local roots (Finding 3 fix, repo-review-2026-07-06/01-gc-memory-safety.md)
// ---------------------------------------------------------------------------
//
// `parse_result`'s E arm used to hold `result`/`continuation`/`union_ptr`/
// `tag_ptr`/`request` as bare `*mut u8` locals across multiple `force_ptr`
// calls (each a GC point via `heap_force`) with zero `register_rust_root`
// calls — a GC landing between two of those forces could relocate an
// already-resolved pointer this function was still holding, and the stale
// value would later be dereferenced (or handed out in `Yield::Request` and
// resumed against, live in the hot effect-dispatch path). `apply_cont_heap`
// already had the right discipline (mark → register → force → truncate) but
// as hand-rolled boilerplate at every call site. These two guards make that
// discipline the default instead of a per-site ritual — used in both.

/// A single heap pointer kept live across GC-capable operations (`force_ptr`,
/// closure calls) for as long as this guard is alive.
///
/// The registered root slot is a stable heap cell (`Box<*mut u8>`), NOT this
/// guard's own stack address — the guard itself may be moved (e.g. returned
/// by value) after construction, which would strand a root registered
/// against its stack address. `get`/`set` always go through the cell, so a
/// GC that relocates the pointee is visible immediately, never a stale
/// snapshot.
///
/// Guards must be dropped in the reverse of their creation order (ordinary
/// Rust scoping already guarantees this for plain `let` locals) — the
/// underlying root stack is LIFO, mirroring `register_rust_root`'s own
/// scoping contract.
struct RootedLocal {
    cell: Box<*mut u8>,
    vmctx: *mut VMContext,
    mark: usize,
}

impl RootedLocal {
    /// Register `ptr` as a Rust GC root.
    ///
    /// # Safety
    /// `vmctx` must be a valid, live `VMContext` for this guard's entire
    /// lifetime.
    unsafe fn new(vmctx: *mut VMContext, ptr: *mut u8) -> Self {
        let mark = crate::host_fns::rust_roots_mark(vmctx);
        let mut cell = Box::new(ptr);
        // SAFETY: `cell`'s heap allocation is stable regardless of where this
        // `RootedLocal` itself ends up (moved, returned, etc.) — only the
        // `Box` handle moves, never its target.
        unsafe {
            crate::host_fns::register_rust_root(vmctx, &mut *cell as *mut *mut u8);
        }
        RootedLocal { cell, vmctx, mark }
    }

    /// The current (GC-updated) pointer value.
    fn get(&self) -> *mut u8 {
        *self.cell
    }

    /// Overwrite the rooted value in place (e.g. after forcing to WHNF) —
    /// the root slot itself is unchanged, so no re-registration is needed.
    fn set(&mut self, ptr: *mut u8) {
        *self.cell = ptr;
    }
}

impl Drop for RootedLocal {
    fn drop(&mut self) {
        // SAFETY: `vmctx` is the live machine this guard was created against
        // (constructor contract); truncating to `mark` is safe as long as
        // guards drop in reverse creation order (type-level doc contract).
        unsafe {
            crate::host_fns::truncate_rust_roots(self.vmctx, self.mark);
        }
    }
}

/// Registers every currently-live entry of a `Vec<*mut u8>` (e.g. the pending
/// k2-continuation stack) as a GC root for this guard's lifetime.
///
/// Holds `&mut Vec` for that lifetime: the borrow checker itself then forbids
/// any push/pop on the stack while the guard is alive, which is exactly the
/// "not pushed/popped while registered" invariant the manual mark/register/
/// truncate blocks this replaces used to rely on hand-audited sequencing for
/// — reallocating the Vec while its element addresses are registered as root
/// slots would strand those roots.
struct RootedStack<'a> {
    vmctx: *mut VMContext,
    mark: usize,
    /// Never read — its sole job is holding the exclusive borrow that makes
    /// concurrent push/pop a compile error for as long as this guard lives.
    #[allow(dead_code)]
    stack: &'a mut Vec<*mut u8>,
}

impl<'a> RootedStack<'a> {
    /// # Safety
    /// `vmctx` must be a valid, live `VMContext` for this guard's entire
    /// lifetime.
    unsafe fn new(vmctx: *mut VMContext, stack: &'a mut Vec<*mut u8>) -> Self {
        let mark = crate::host_fns::rust_roots_mark(vmctx);
        for slot in stack.iter_mut() {
            // SAFETY: slot is a valid, non-moving address for this guard's
            // lifetime (the borrow above prevents reallocation).
            unsafe {
                crate::host_fns::register_rust_root(vmctx, slot as *mut *mut u8);
            }
        }
        RootedStack { vmctx, mark, stack }
    }
}

impl Drop for RootedStack<'_> {
    fn drop(&mut self) {
        // SAFETY: same contract as `RootedLocal::drop`.
        unsafe {
            crate::host_fns::truncate_rust_roots(self.vmctx, self.mark);
        }
    }
}

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
    /// The unqualified constructor name as it appears in the DataConTable.
    pub fn name(self) -> &'static str {
        match self {
            EffContKind::Val => "Val",
            EffContKind::E => "E",
            EffContKind::Union => "Union",
            EffContKind::Leaf => "Leaf",
            EffContKind::Node => "Node",
        }
    }

    /// The module-qualified constructor name as recorded in the DataConTable
    /// (`Module.Ctor`, via `Tidepool.Translate.qualifiedName`).
    ///
    /// These are fixed: the freer-simple / open-union / FTCQueue packages are
    /// pinned in the toolchain, so the defining modules never move. This is the
    /// reliable discriminator when a user import collides on the unqualified
    /// name — e.g. `Data.Tree.Node` shadows the FTCQueue continuation `Node`,
    /// both at arity 2, so unqualified-name and arity lookup are both ambiguous.
    pub fn qualified_name(self) -> &'static str {
        match self {
            EffContKind::Val => "Control.Monad.Freer.Val",
            EffContKind::E => "Control.Monad.Freer.E",
            EffContKind::Union => "Data.OpenUnion.Union",
            EffContKind::Leaf => "Data.FTCQueue.Leaf",
            EffContKind::Node => "Data.FTCQueue.Node",
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
        // Resolve each freer continuation constructor. Prefer the module-qualified
        // name: it is unambiguous even when a user import collides on the
        // unqualified name (e.g. `Data.Tree.Node` vs the FTCQueue continuation
        // `Data.FTCQueue.Node`, both arity 2 — `get_by_name` returns `None` for
        // such collisions, and arity does not disambiguate). Fall back to the
        // unqualified lookup only when the qualified name is absent, preserving
        // the no-collision behaviour for any producer that omits qualified names.
        //
        // The (qualified, bare) pairs are the SAME toolchain-pinned names the
        // oracle resolves against (`tidepool_effect::machine::EffectMachine::new`)
        // — hoisted as shared `pub` consts in `tidepool_effect::freer_names` so
        // the two resolution schemes cannot drift apart (#F5).
        let resolve =
            |kind: EffContKind, qualified: &str, bare: &str| -> Result<u64, EffContKind> {
                tidepool_effect::freer_names::resolve(table, qualified, bare)
                    .map(|t| t.0)
                    .ok_or(kind)
            };
        Ok(ConTags {
            val: resolve(
                EffContKind::Val,
                tidepool_effect::freer_names::VAL_QUALIFIED,
                tidepool_effect::freer_names::VAL,
            )?,
            e: resolve(
                EffContKind::E,
                tidepool_effect::freer_names::E_QUALIFIED,
                tidepool_effect::freer_names::E,
            )?,
            union: resolve(
                EffContKind::Union,
                tidepool_effect::freer_names::UNION_QUALIFIED,
                tidepool_effect::freer_names::UNION,
            )?,
            leaf: resolve(
                EffContKind::Leaf,
                tidepool_effect::freer_names::LEAF_QUALIFIED,
                tidepool_effect::freer_names::LEAF,
            )?,
            node: resolve(
                EffContKind::Node,
                tidepool_effect::freer_names::NODE_QUALIFIED,
                tidepool_effect::freer_names::NODE,
            )?,
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
        crate::debug::init_logging();
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

        // Force result if it's a thunk (lazy Con field from parent). Forcing runs
        // JIT code, which can raise a runtime error AFTER the check above — e.g. a
        // top-level result that is itself a thunk (`pure (go 500000)`) drives the
        // whole computation here, and the recursion-depth guard sets StackOverflow
        // and returns the tag-0 poison closure. Re-check so that error is surfaced
        // instead of being masked by the poison's tag in the TAG_CON check below.
        let result = self.force_ptr(result);
        if let Some(err) = crate::host_fns::take_runtime_error() {
            return Yield::Error(YieldError::from(err));
        }
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
            // Force value field — it may be a thunk. Forcing runs JIT code that
            // can raise a runtime error; surface it rather than handing the tag-0
            // poison object to the caller (which would mis-render it or hand it to
            // the heap bridge). Same invariant as the post-force re-check above.
            let value = self.force_ptr(value);
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Yield::Error(YieldError::from(err));
            }
            Yield::Done(value)
        } else if con_tag == self.tags.e {
            // E(union, continuation) — extract Union and k
            let num_fields = unsafe { Self::read_con_num_fields(result) };
            if num_fields != 2 {
                return Yield::Error(YieldError::BadEFields(num_fields));
            }
            let vmctx = &mut self.vmctx as *mut VMContext;
            let union_field = unsafe { Self::read_con_field(result, 0) };
            let cont_field = unsafe { Self::read_con_field(result, 1) };
            // Root BOTH before forcing either: `heap_force` can GC, and an
            // unrooted bare local sitting in this host frame is invisible to
            // the frame walker — forcing one could relocate the other.
            let mut union_ptr = unsafe { RootedLocal::new(vmctx, union_field) };
            let mut continuation = unsafe { RootedLocal::new(vmctx, cont_field) };

            // Force all field pointers — they may be thunks from lazy Con fields
            union_ptr.set(self.force_ptr(union_ptr.get()));
            if union_ptr.get().is_null() {
                return Yield::Error(YieldError::NullPointer);
            }
            continuation.set(self.force_ptr(continuation.get()));
            if continuation.get().is_null() {
                return Yield::Error(YieldError::NullPointer);
            }

            let union_tag = unsafe { *union_ptr.get() };
            if union_tag != layout::TAG_CON {
                return Yield::Error(YieldError::UnexpectedTag(union_tag));
            }

            let union_num_fields = unsafe { Self::read_con_num_fields(union_ptr.get()) };
            if union_num_fields != 2 {
                return Yield::Error(YieldError::BadUnionFields(union_num_fields));
            }

            let tag_field = unsafe { Self::read_con_field(union_ptr.get(), 0) };
            let mut tag_ptr = unsafe { RootedLocal::new(vmctx, tag_field) };
            tag_ptr.set(self.force_ptr(tag_ptr.get()));
            if tag_ptr.get().is_null() {
                return Yield::Error(YieldError::NullPointer);
            }
            // Read the actual effect tag value. The Union's first field is the
            // position index (Word#). After Core normalization (Rule 2),
            // this is ideally an unboxed Lit(Word, N). However, we maintain
            // a fallback for boxed W# to handle cross-module variables that
            // normalization cannot safely unbox.
            let tag_ptr_tag = unsafe { *tag_ptr.get() };
            // core-shapes.md §7: effect tag should be unboxed Lit after normalization,
            // but we must handle boxed W# for cross-module variables Rule 2 can't see.
            if tag_ptr_tag != layout::TAG_LIT && tag_ptr_tag != layout::TAG_CON {
                return Yield::Error(YieldError::UnexpectedTag(tag_ptr_tag));
            }

            let effect_tag = if tag_ptr_tag == layout::TAG_LIT {
                unsafe { *(tag_ptr.get().add(layout::LIT_VALUE_OFFSET as usize) as *const u64) }
            } else {
                // Fallback for boxed W#: Read the LitWord from field 0.
                // Harden: verify it's a TAG_CON and has at least one field.
                if tag_ptr_tag != layout::TAG_CON {
                    return Yield::Error(YieldError::UnexpectedTag(tag_ptr_tag));
                }
                let num_fields = unsafe { Self::read_con_num_fields(tag_ptr.get()) };
                if num_fields == 0 {
                    return Yield::Error(YieldError::UnexpectedTag(tag_ptr_tag));
                }

                let lit_field = unsafe { Self::read_con_field(tag_ptr.get(), 0) };
                let mut lit_ptr = unsafe { RootedLocal::new(vmctx, lit_field) };
                lit_ptr.set(self.force_ptr(lit_ptr.get()));
                if lit_ptr.get().is_null() {
                    return Yield::Error(YieldError::NullPointer);
                }
                let lit_ptr_tag = unsafe { *lit_ptr.get() };
                if lit_ptr_tag != layout::TAG_LIT {
                    return Yield::Error(YieldError::UnexpectedTag(lit_ptr_tag));
                }
                unsafe { *(lit_ptr.get().add(layout::LIT_VALUE_OFFSET as usize) as *const u64) }
                // lit_ptr drops here — innermost guard, created last, freed first.
            };
            let request_field = unsafe { Self::read_con_field(union_ptr.get(), 1) };
            let mut request = unsafe { RootedLocal::new(vmctx, request_field) };
            request.set(self.force_ptr(request.get()));

            log::debug!(
                target: "tidepool::effects",
                "effect_tag={} tag_ptr_tag={} union_con_tag={} request_tag={}",
                effect_tag,
                tag_ptr_tag,
                unsafe { Self::read_con_tag(union_ptr.get()) },
                if request.get().is_null() {
                    255
                } else {
                    unsafe { *request.get() }
                }
            );

            Yield::Request {
                tag: effect_tag,
                request: request.get(),
                continuation: continuation.get(),
            }
            // Guards drop here in reverse creation order: request, tag_ptr,
            // continuation, union_ptr — matching the root stack's LIFO
            // discipline.
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
            } else if matches!(tag, layout::TAG_CON | layout::TAG_LIT | layout::TAG_CLOSURE) {
                return current;
            } else {
                let msg = format!(
                    "force_ptr: unexpected heap tag {} (not a thunk, con, lit, or closure)",
                    tag
                );
                crate::host_fns::push_diagnostic(msg.clone());
                crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64); // 2 = UserError
                return crate::host_fns::error_poison_ptr();
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

        // GC-cluster reach (leaf 3): all rust-root register/mark/truncate
        // calls in this function key on this machine's own vmctx.
        let vmctx = &mut self.vmctx as *mut VMContext;

        // `k`/`arg` are reassigned across loop iterations (Node descent,
        // Val/k2 resumption) but never need re-registration: `RootedLocal`'s
        // cell is a stable heap slot, so `.set()` on reassignment keeps the
        // SAME root live for the whole call — rooted unconditionally in
        // every branch below rather than per-site as the manual discipline
        // this replaces required.
        let mut k = unsafe { RootedLocal::new(vmctx, k) };
        let mut arg = unsafe { RootedLocal::new(vmctx, arg) };

        k.set(self.force_ptr(k.get()));
        let entry_err = k.get().is_null() || crate::host_fns::has_runtime_error();
        if !entry_err {
            arg.set(self.force_ptr(arg.get()));
        }
        if entry_err || crate::host_fns::has_runtime_error() {
            return std::ptr::null_mut();
        }

        // Stack of pending k2 continuations from Node decomposition. Lives on
        // the Rust heap, not the GC nursery. `RootedStack` (created fresh
        // around each risk point below) roots every CURRENT entry; holding
        // `&mut k2_stack` for its lifetime makes "not pushed/popped while
        // registered" a borrow-checker guarantee instead of a hand-audited
        // invariant.
        let mut k2_stack: Vec<*mut u8> = Vec::new();

        loop {
            if k.get().is_null() {
                return std::ptr::null_mut();
            }

            let tag = *k.get();
            let result_raw = match tag {
                t if t == layout::TAG_CON => {
                    let con_tag = Self::read_con_tag(k.get());

                    if con_tag == self.tags.leaf {
                        // Leaf(f): call f(arg) — terminal for this continuation.
                        // Forcing f and calling it can both GC; k2_stack (and
                        // arg, always rooted above) must stay visible through
                        // both.
                        let _roots = unsafe { RootedStack::new(vmctx, &mut k2_stack) };
                        let f = self.force_ptr(Self::read_con_field(k.get(), 0));
                        if crate::host_fns::has_runtime_error() {
                            return std::ptr::null_mut();
                        }
                        self.call_closure(f, arg.get())
                    } else if con_tag == self.tags.node {
                        // Node(k1, k2): push k2 for later, loop on k1. The
                        // first force can GC and move `k`/`arg` (always
                        // rooted); the second can move `k1`.
                        let (k1_val, k2_val) = {
                            let _roots = unsafe { RootedStack::new(vmctx, &mut k2_stack) };
                            let mut k1 = unsafe {
                                RootedLocal::new(vmctx, Self::read_con_field(k.get(), 0))
                            };
                            k1.set(self.force_ptr(k1.get()));
                            let k2_val = self.force_ptr(Self::read_con_field(k.get(), 1));
                            (k1.get(), k2_val)
                        };
                        if crate::host_fns::has_runtime_error() {
                            return std::ptr::null_mut();
                        }
                        k2_stack.push(k2_val);
                        k.set(k1_val);
                        continue;
                    } else {
                        // core-shapes.md §7: continuation must be Leaf or Node
                        let msg = format!(
                            "apply_cont_heap: unexpected continuation con_tag {} (expected Leaf or Node)",
                            con_tag
                        );
                        crate::host_fns::push_diagnostic(msg.clone());
                        crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64); // 2 = UserError
                        return std::ptr::null_mut();
                    }
                }
                t if t == layout::TAG_CLOSURE => {
                    // Raw closure (degenerate continuation fallback). `arg` is
                    // always rooted above.
                    let _roots = unsafe { RootedStack::new(vmctx, &mut k2_stack) };
                    self.call_closure(k.get(), arg.get())
                }
                _ => {
                    let msg = format!(
                        "apply_cont_heap: unexpected heap tag {} in continuation position (expected TAG_CON or TAG_CLOSURE)",
                        tag
                    );
                    crate::host_fns::push_diagnostic(msg.clone());
                    crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64); // 2 = UserError
                    return std::ptr::null_mut();
                }
            };

            // We have a result from call_closure. Compose with pending k2s.
            if result_raw.is_null() {
                // core-shapes.md §7: closure application must return a valid result
                let msg =
                    "apply_cont_heap: closure application returned null (expected Eff result)";
                crate::host_fns::push_diagnostic(msg.to_string());
                crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64); // 2 = UserError
                return std::ptr::null_mut();
            }
            // `result` persists (rooted) for the rest of this iteration: the
            // Val branch reads its field AFTER forcing y, and the E branch
            // reads field 1 AFTER forcing field 0 — both need `result` itself
            // kept live and up to date across an intervening GC.
            let mut result = unsafe { RootedLocal::new(vmctx, result_raw) };
            {
                // Forcing the Eff result can GC: protect the pending k2s too.
                let _roots = unsafe { RootedStack::new(vmctx, &mut k2_stack) };
                result.set(self.force_ptr(result.get()));
            }
            if result.get().is_null() || crate::host_fns::has_runtime_error() {
                // core-shapes.md §7: forced result must be non-null (unless error set)
                if !crate::host_fns::has_runtime_error() {
                    let msg = "apply_cont_heap: forced result is null (expected Eff result)";
                    crate::host_fns::push_diagnostic(msg.to_string());
                    crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
                    // 2 = UserError
                }
                return std::ptr::null_mut();
            }

            let result_tag = *result.get();
            if result_tag != layout::TAG_CON {
                // core-shapes.md §7: Eff result must be TAG_CON
                let msg = format!(
                    "apply_cont_heap: result has unexpected tag {} (expected TAG_CON for Eff result)",
                    result_tag
                );
                crate::host_fns::push_diagnostic(msg.clone());
                crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64); // 2 = UserError
                return std::ptr::null_mut();
            }

            let result_con_tag = Self::read_con_tag(result.get());

            if result_con_tag == self.tags.val {
                // Val(y): if k2_stack is empty, we're done; otherwise apply next k2.
                // Forcing y can GC (e.g. it is a lazy effect-result tail thunk
                // materializing a chunk): `result` (returned below) and the
                // pending k2s must stay visible across it.
                let y = {
                    let _roots = unsafe { RootedStack::new(vmctx, &mut k2_stack) };
                    self.force_ptr(Self::read_con_field(result.get(), 0))
                };
                if crate::host_fns::has_runtime_error() {
                    return std::ptr::null_mut();
                }
                if let Some(k2) = k2_stack.pop() {
                    k.set(k2);
                    arg.set(y);
                    continue;
                } else {
                    return result.get();
                }
            } else if result_con_tag == self.tags.e {
                // E(union, k'): compose ALL remaining k2s into k'. Both forces
                // can GC: `result` (field 1 is read after the first force),
                // `union_val` across the second force, and the pending k2s
                // all need protecting. The alloc_con composition below is
                // bump-only (null on exhaustion), so no protection is needed
                // past here.
                let (union_val, mut k_prime) = {
                    let _roots = unsafe { RootedStack::new(vmctx, &mut k2_stack) };
                    let mut union_val =
                        unsafe { RootedLocal::new(vmctx, Self::read_con_field(result.get(), 0)) };
                    union_val.set(self.force_ptr(union_val.get()));
                    let k_prime = self.force_ptr(Self::read_con_field(result.get(), 1));
                    (union_val.get(), k_prime)
                };
                if crate::host_fns::has_runtime_error() {
                    return std::ptr::null_mut();
                }

                while let Some(k2) = k2_stack.pop() {
                    k_prime = self.alloc_con(self.tags.node, &[k_prime, k2]);
                    if k_prime.is_null() {
                        // alloc_con failed (OOM)
                        let msg = "apply_cont_heap: failed to allocate Node during continuation composition";
                        crate::host_fns::push_diagnostic(msg.to_string());
                        crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
                        return std::ptr::null_mut();
                    }
                }
                let res = self.alloc_con(self.tags.e, &[union_val, k_prime]);
                if res.is_null() {
                    let msg = "apply_cont_heap: failed to allocate E result during continuation composition";
                    crate::host_fns::push_diagnostic(msg.to_string());
                    crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
                }
                return res;
            } else {
                // core-shapes.md §7: Eff result must be Val or E
                let msg = format!(
                    "apply_cont_heap: result con_tag {} is neither Val nor E",
                    result_con_tag
                );
                crate::host_fns::push_diagnostic(msg.clone());
                crate::host_fns::runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64); // 2 = UserError
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

        if log::log_enabled!(target: "tidepool::calls", log::Level::Trace) {
            let name = crate::debug::lookup_lambda(code_ptr)
                .unwrap_or_else(|| format!("0x{:x}", code_ptr));
            log::trace!(
                target: "tidepool::calls",
                "call_closure {} closure={:?} arg={}",
                name,
                closure,
                crate::debug::heap_describe(arg),
            );
        }
        if log::log_enabled!(target: "tidepool::heap", log::Level::Trace) {
            if let Err(e) = crate::debug::heap_validate_deep(closure) {
                log::trace!(target: "tidepool::heap", "INVALID closure: {}", e);
                log::trace!(target: "tidepool::heap", "  {}", crate::debug::heap_describe(closure));
                return std::ptr::null_mut();
            }
            if let Err(e) = crate::debug::heap_validate(arg) {
                log::trace!(target: "tidepool::heap", "INVALID arg: {}", e);
                return std::ptr::null_mut();
            }
            // Dump captures
            let num_captured =
                *(closure.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16);
            for i in 0..num_captured as usize {
                let cap = *(closure.add(layout::CLOSURE_CAPTURED_OFFSET as usize + 8 * i)
                    as *const *const u8);
                if cap.is_null() {
                    log::trace!(target: "tidepool::heap", "  capture[{}] = NULL", i);
                } else {
                    log::trace!(
                        target: "tidepool::heap",
                        "  capture[{}] = {}",
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

        if log::log_enabled!(target: "tidepool::calls", log::Level::Trace) {
            let name = crate::debug::lookup_lambda(code_ptr)
                .unwrap_or_else(|| format!("0x{:x}", code_ptr));
            if result.is_null() {
                log::trace!(target: "tidepool::calls", "{} returned NULL", name);
            } else {
                log::trace!(
                    target: "tidepool::calls",
                    "{} returned {}",
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
            if crate::host_fns::check_cancel_and_set_error(&mut self.vmctx as *mut VMContext) {
                self.vmctx.tail_callee = std::ptr::null_mut();
                self.vmctx.tail_arg = std::ptr::null_mut();
                *result = crate::host_fns::error_poison_ptr();
                return;
            }

            let callee = self.vmctx.tail_callee;
            let arg = self.vmctx.tail_arg;
            self.vmctx.tail_callee = std::ptr::null_mut();
            self.vmctx.tail_arg = std::ptr::null_mut();
            unsafe { machine_state(&mut self.vmctx as *mut VMContext) }.reset_call_depth();
            let code_ptr = *(callee.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
            let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
                std::mem::transmute(code_ptr);
            *result = func(&mut self.vmctx, callee, arg);
        }
    }

    /// Allocate a Con HeapObject on the nursery with the given tag and fields.
    unsafe fn alloc_con(&mut self, con_tag: u64, fields: &[*mut u8]) -> *mut u8 {
        // The header stores size and num_fields as u16. Unbounded, `size as
        // u16` wraps at >= 8189 fields while num_fields stays correct — GC
        // evacuation then copies the wrapped size (fields LOST) and the
        // cheney scan walks into garbage (proptest_heap_layout C2). Refuse at
        // the bound the read side enforces; the null return routes through
        // the caller's existing OOM/poison handling.
        if fields.len() > heap_bridge::MAX_FIELDS {
            return std::ptr::null_mut();
        }
        // SAFETY: Bump-allocating from vmctx nursery. Writing Con header, tag,
        // num_fields, and field pointers at known layout offsets within the allocation.
        let size = 24 + 8 * fields.len();
        let ptr = heap_bridge::bump_alloc_from_vmctx(&mut self.vmctx, size);
        if ptr.is_null() {
            return std::ptr::null_mut();
        }
        heap_layout::write_header(ptr, layout::TAG_CON, size as u32);
        *(ptr.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = con_tag;
        *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = fields.len() as u16;
        for (i, &fp) in fields.iter().enumerate() {
            *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * i) as *mut *mut u8) = fp;
        }
        ptr
    }
}
