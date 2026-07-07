//! WHNF/NF forcing (`heap_force`, `deep_force`) and the tail-call trampoline.

use crate::context::VMContext;
use crate::effect_machine::RootedLocal;
use crate::layout;
use crate::machine_state::machine_state;
use rustc_hash::FxHashSet;
use tidepool_heap::layout as heap_layout;

use super::cancel::check_cancel_and_set_error;
use super::errors::{
    error_poison_ptr, has_runtime_error, overwrite_runtime_error, runtime_bad_thunk_state_trap,
    runtime_blackhole_trap, RuntimeError,
};
use super::gc::{register_rust_root, rust_roots_mark, truncate_rust_roots};

/// Upper bound on consecutive EVALUATED-indirection follows in one
/// `heap_force` call. A genuine chain needs one distinct (>=48-byte) thunk per
/// link — 64M links would need >3 GiB of thunks, beyond any heap we run — so
/// exceeding it can only mean a memoized indirection cycle (#336).
const INDIRECTION_FOLLOW_LIMIT: u64 = 64 * 1024 * 1024;

/// Force a thunk to WHNF. Loops to handle chains (thunk returning thunk).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn heap_force(vmctx: *mut VMContext, obj: *mut u8) -> *mut u8 {
    if obj.is_null() {
        return obj;
    }

    // SAFETY: obj is a valid heap pointer from the JIT nursery. The loop follows
    // indirection chains (thunks) and calls thunk entry functions via transmuted
    // code pointers stored in the thunk object. vmctx is passed through from JIT.
    unsafe {
        let mut current = obj;
        let mut follow_steps: u64 = 0;

        loop {
            let tag = heap_layout::read_tag(current);

            if tag == layout::TAG_THUNK {
                let state = *current.add(layout::THUNK_STATE_OFFSET as usize);
                match state {
                    layout::THUNK_UNEVALUATED => {
                        // 1. Mark blackhole for cycle detection
                        *current.add(layout::THUNK_STATE_OFFSET as usize) = layout::THUNK_BLACKHOLE;

                        // 2. Read code pointer
                        let code_ptr =
                            *(current.add(layout::THUNK_CODE_PTR_OFFSET as usize) as *const usize);

                        if code_ptr == 0 {
                            overwrite_runtime_error(RuntimeError::NullFunPtr);
                            return error_poison_ptr();
                        }

                        // 3. Call thunk entry function
                        // Signature: fn(vmctx, thunk_ptr) -> whnf_ptr
                        //
                        // The thunk code can allocate and trigger GC. `current` lives
                        // in this host frame, which the JIT frame walker deliberately
                        // skips, so it must be registered as an explicit Rust root:
                        // the copying GC frees from-space at the end of every
                        // collection, so a stale `current` would dangle into freed
                        // memory (post-call forwarding checks are unsound).
                        let f: extern "C" fn(*mut VMContext, *mut u8) -> *mut u8 =
                            std::mem::transmute(code_ptr);
                        let mark = rust_roots_mark(vmctx);
                        register_rust_root(vmctx, &mut current as *mut *mut u8);
                        let result = f(vmctx, current);
                        truncate_rust_roots(vmctx, mark);

                        // L1 (repo-review-2026-07-06/01-gc-memory-safety.md,
                        // Low findings): a JIT call chain (App's
                        // null_propagate_block / trampoline_resolve's
                        // defensive "shouldn't happen" paths) can return null
                        // WITHOUT setting has_runtime_error. Memoizing that
                        // null as this thunk's indirection (the branch below
                        // would otherwise do exactly that) leaves a null
                        // pointer for a LATER force to dereference when it
                        // follows THUNK_EVALUATED's indirection — segfault.
                        // Guard it exactly like the code_ptr==0 case above:
                        // record a real error and memoize the poison object
                        // (never null) instead.
                        if result.is_null() {
                            overwrite_runtime_error(RuntimeError::BadPointer);
                            *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize)
                                as *mut *mut u8) = error_poison_ptr();
                            *current.add(layout::THUNK_STATE_OFFSET as usize) =
                                layout::THUNK_EVALUATED;
                            return error_poison_ptr();
                        }

                        // If the thunk body raised an error (e.g. HeapOverflow
                        // from runtime_oom), memoize the poison result so
                        // re-forces follow the indirection instead of
                        // re-entering the failed body (which would GC-thrash
                        // until SIGSEGV). Then return poison immediately —
                        // don't loop into further forces.
                        if has_runtime_error() {
                            *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize)
                                as *mut *mut u8) = result;
                            *current.add(layout::THUNK_STATE_OFFSET as usize) =
                                layout::THUNK_EVALUATED;
                            return error_poison_ptr();
                        }

                        debug_assert_ne!(
                            heap_layout::read_tag(current),
                            layout::TAG_FORWARDED,
                            "heap_force: registered root left forwarded"
                        );

                        // A body that returns the very thunk being forced is a
                        // value cycle (`let x = x`): memoizing it would write a
                        // self-indirection and ERASE the blackhole, turning the
                        // <<loop>> into an infinite EVALUATED-follow spin (#336).
                        // Memoize the poison instead so re-forces fail fast.
                        if result == current {
                            let poison = runtime_blackhole_trap(vmctx);
                            *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize)
                                as *mut *mut u8) = poison;
                            *current.add(layout::THUNK_STATE_OFFSET as usize) =
                                layout::THUNK_EVALUATED;
                            return poison;
                        }

                        // 4. Write indirection (offset 16, overwriting code_ptr)
                        *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *mut *mut u8) =
                            result;

                        // 5. Set state = Evaluated
                        *current.add(layout::THUNK_STATE_OFFSET as usize) = layout::THUNK_EVALUATED;

                        // Result may be another thunk — loop to force it
                        current = result;
                        continue;
                    }
                    layout::THUNK_BLACKHOLE => {
                        return runtime_blackhole_trap(vmctx);
                    }
                    layout::THUNK_EVALUATED => {
                        // Mutual aliases (`x = y; y = x`) memoize an
                        // EVALUATED indirection CYCLE that contains no
                        // blackhole state to trap on (#336). A legitimate
                        // chain is bounded by how many thunks fit in the heap
                        // (each link is a distinct >=48-byte thunk), so a
                        // follow count past the limit can only be a cycle.
                        follow_steps += 1;
                        if follow_steps > INDIRECTION_FOLLOW_LIMIT {
                            return runtime_blackhole_trap(vmctx);
                        }
                        let next = *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize)
                            as *const *mut u8);
                        current = next;
                        continue;
                    }
                    other => return runtime_bad_thunk_state_trap(vmctx, other),
                }
            }

            // Non-thunk tags (Closure, Con, Lit, unknown) — already WHNF.
            // Note: the pre-thunk closure-forcing path was removed because
            // TAG_THUNK now handles all lazy computations. TAG_CLOSURE objects
            // are genuine lambdas (with captures/args) and must not be called
            // with null arguments.
            return current;
        }
    }
}

/// Pointer stride of a `Con` field slot (one machine word). Matches the
/// `8 * index` field arithmetic in `effect_machine.rs` / `layout` Con reads.
const CON_FIELD_PTR_STRIDE: usize = 8;

/// How many work items `deep_force` processes between external-cancellation
/// checks (M3). A fully-evaluated structure (every field already a Con/Lit,
/// nothing left to actually force) never triggers a GC and so never crosses
/// `heap_force`'s own cancel-adjacent safepoints — for a large such structure
/// (e.g. an already-forced million-element list handed back to `deep_force`
/// again) the old loop had NO cancel check anywhere in it and was, in
/// practice, unkillable until it walked the whole graph. This is a plain
/// counter check (no GC point), so the interval can be small without being
/// a meaningful cost center.
const CANCEL_CHECK_INTERVAL: u32 = 4096;

/// Force a heap value to **normal form** (NF), iteratively (Wave 1.B, component K).
///
/// Unlike [`heap_force`] (WHNF — stops at the outermost constructor), this drives
/// the *entire* first-order (Tier-0) data spine to NF: it forces each node to
/// WHNF, then descends into every `Con` field and forces those too, writing the
/// forced pointer back into the field so the resulting graph holds no
/// unevaluated thunks. Closures / PAPs (`TAG_CLOSURE`) are **Tier-1**: forced to
/// WHNF but NOT descended into — a closure is a legitimate stored value, and
/// deep-forcing its captured environment has no NF meaning (and could diverge).
/// `Lit` leaves are already NF.
///
/// Iterative with an explicit work stack (no host recursion) — mirrors
/// `tidepool-eval`'s `deep_force` and the GC's `cheney_copy`, so an arbitrarily
/// deep structure (long list, deep tree) cannot overflow the host stack.
///
/// GC-safety: forcing a thunk runs JIT code that can allocate and trigger a
/// collection, relocating live objects. Each work item roots its own parent
/// pointer via a [`RootedLocal`] (registered once when pushed, truncated once
/// when popped — see the M3 doc block below for why this replaced a
/// per-iteration full-stack re-registration), so the copying GC rewrites it
/// in place and no pending pointer dangles. A field slot is recomputed from
/// its (possibly relocated) parent *after* the force, never cached across it.
///
/// ## M3 (repo-review-2026-07-06/01-gc-memory-safety.md, Medium findings)
///
/// Three fixes over the original version, all in this one function:
///
/// - **O(n²) root re-registration**: the old loop re-registered EVERY still-
///   pending work item as a root on EVERY iteration (a fresh
///   register-then-immediately-truncate scan over the whole remaining
///   stack), because a work item was a bare `*mut u8` inside a `Vec` that
///   reallocates as it grows — any root registered at a raw address into
///   that `Vec`'s backing buffer would dangle across a later `push`. Each
///   item now carries its OWN [`RootedLocal`] (a heap-stable `Box` cell,
///   immune to the outer `Vec` reallocating) registered exactly once at push
///   time and dropped (truncating exactly that one registration) exactly
///   once at pop time — O(1) amortized per item instead of O(n) per pop.
///   This relies on `work` behaving as a strict LIFO stack (push child items
///   only after popping+finishing their parent item), which keeps the
///   per-item registrations perfectly nested with the global rust_roots
///   stack; do not reorder pops/pushes without re-checking that invariant.
/// - **No visited set (exponential blowup on shared DAGs)**: a value like
///   `iterate (\v -> (v,v)) x !! 40` shares the SAME sub-object from both
///   fields of every level, so an unforced traversal re-descends into it at
///   every level — 2^40 for a depth-40 tower. `visited` (keyed by object
///   ADDRESS) skips re-pushing a `Con`'s fields once already queued. Because
///   the GC can relocate objects (and later reuse a vacated address for
///   something unrelated), a raw address-keyed set is only trustworthy
///   between two points with no collection in between: `gc_generation()` is
///   snapshotted around every [`heap_force`] call, and any change clears
///   `visited` entirely rather than risk a false "already visited" hit on a
///   coincidentally-reused address. This only ever costs a redundant (but
///   bounded, non-exponential) re-descent right after a collection, never a
///   correctness bug.
/// - **No cancel safepoint**: see [`CANCEL_CHECK_INTERVAL`].
///
/// Returns the (possibly relocated) NF root pointer, or the error poison pointer
/// if forcing raised a runtime error.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deep_force(vmctx: *mut VMContext, root: *mut u8) -> *mut u8 {
    if root.is_null() {
        return root;
    }
    // SAFETY: `root` is a valid heap pointer; heap_force + the layout reads below
    // operate on valid heap objects; the rooting protocol (see doc comment) keeps
    // every pending pointer live and GC-updated across each collection.
    unsafe {
        // Force the root to WHNF first. heap_force roots its own argument across
        // the call, so a GC here is safe with no pending work items yet.
        let mut nf_root = heap_force(vmctx, root);
        if has_runtime_error() {
            return error_poison_ptr();
        }

        // Keep the NF root registered for the WHOLE descent: once a child is
        // popped off the work stack it is reachable only through the root graph,
        // so the root must stay live (and GC-updated) until we return it.
        let base_mark = rust_roots_mark(vmctx);
        register_rust_root(vmctx, &mut nf_root as *mut *mut u8);

        // Address-keyed dedup for shared sub-graphs (M3); see the doc block.
        let mut visited: FxHashSet<usize> = FxHashSet::default();

        // Work items are (rooted parent pointer, field index). The field index
        // is stable, so the field slot is recomputed from the live (GC-updated)
        // parent after each force, never cached across it.
        let mut work: Vec<(RootedLocal, usize)> = Vec::new();
        push_con_fields(vmctx, nf_root, &mut work, &mut visited);

        let mut since_cancel_check: u32 = 0;
        while let Some((parent_root, idx)) = work.pop() {
            since_cancel_check += 1;
            if since_cancel_check >= CANCEL_CHECK_INTERVAL {
                since_cancel_check = 0;
                if check_cancel_and_set_error(vmctx) {
                    drop(parent_root);
                    truncate_rust_roots(vmctx, base_mark);
                    return error_poison_ptr();
                }
            }

            // Read the child from the live (possibly-relocated-by-an-earlier-
            // iteration) parent, force it, then write the NF child back.
            let field_off = layout::CON_FIELDS_OFFSET as usize + idx * CON_FIELD_PTR_STRIDE;
            let child = *(parent_root.get().add(field_off) as *const *mut u8);

            let gen_before = machine_state(vmctx).gc_generation();
            let forced_child = heap_force(vmctx, child);

            if has_runtime_error() {
                drop(parent_root);
                truncate_rust_roots(vmctx, base_mark);
                return error_poison_ptr();
            }
            if machine_state(vmctx).gc_generation() != gen_before {
                // A collection ran during this force: every address `visited`
                // remembers may now be stale (moved) or reused by something
                // else entirely — discard it rather than risk a false hit.
                visited.clear();
            }

            // `parent` may have moved during the force; re-read through the
            // still-registered root before writing back.
            *(parent_root.get().add(field_off) as *mut *mut u8) = forced_child;

            // Done with this item: drop its root registration (truncates
            // exactly this one entry — see the LIFO-nesting doc above) BEFORE
            // pushing any of `forced_child`'s own fields.
            drop(parent_root);

            // Descend into Tier-0 data only; Lits are leaves, Closures are Tier-1.
            push_con_fields(vmctx, forced_child, &mut work, &mut visited);
        }

        truncate_rust_roots(vmctx, base_mark);
        nf_root
    }
}

/// Push `(rooted parent, i)` for each field index of a `Con` object onto
/// `work`, registering each as its own GC root. No-op for non-`Con` objects
/// (`Lit` leaves; `Closure`/PAP = Tier-1, not descended) OR an `obj` already
/// present in `visited` (M3: a shared sub-graph is only ever queued once).
///
/// # Safety
/// `obj` must be a valid heap-object pointer; `vmctx` must be a valid, live
/// `VMContext`.
unsafe fn push_con_fields(
    vmctx: *mut VMContext,
    obj: *mut u8,
    work: &mut Vec<(RootedLocal, usize)>,
    visited: &mut FxHashSet<usize>,
) {
    if heap_layout::read_tag(obj) != layout::TAG_CON {
        return;
    }
    if !visited.insert(obj as usize) {
        return;
    }
    let n = *(obj.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) as usize;
    for i in 0..n {
        // SAFETY: vmctx is valid for this call (caller contract).
        let root = unsafe { RootedLocal::new(vmctx, obj) };
        work.push((root, i));
    }
}

/// Resolve pending tail calls from VMContext. Called by non-tail App sites
/// when the callee returned null (indicating a tail call was stored).
///
/// Loop: read tail_callee+tail_arg from VMContext, clear them, call the closure,
/// check if result is null (another tail call) or a real value.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn trampoline_resolve(vmctx: *mut VMContext) -> *mut u8 {
    // SAFETY: vmctx is a valid pointer from JIT code. tail_callee/tail_arg are valid
    // heap pointers set by JIT tail-call sites. Code pointers in closures were set
    // during compilation and point to finalized JIT functions.
    unsafe {
        loop {
            // External cancellation safepoint. Tail-recursive loops never
            // return to the top-level JIT call on their own, so we must check
            // here — otherwise a runaway loop observes the cancel in
            // `gc_trigger`, receives a poison pointer from `runtime_oom`, and
            // immediately re-enters the trampoline forever. Returning the
            // poison here unwinds up to `JitEffectMachine::run_pure`, which
            // then surfaces `RuntimeError::Cancelled`.
            if check_cancel_and_set_error(vmctx) {
                (*vmctx).tail_callee = std::ptr::null_mut();
                (*vmctx).tail_arg = std::ptr::null_mut();
                return error_poison_ptr();
            }

            // Runtime-error safepoint. A tail-recursive loop that exhausts the
            // heap gets a poison pointer from `runtime_oom` (which sets
            // `HeapOverflow`) but, like cancel, never returns to the top-level
            // JIT call on its own — it would re-enter the trampoline forever,
            // GC-thrashing a full heap until it corrupts and SIGSEGVs. Bail the
            // moment any error is pending so the poison unwinds to the run loop,
            // which surfaces the clean `HeapOverflow` (or other) error.
            if has_runtime_error() {
                (*vmctx).tail_callee = std::ptr::null_mut();
                (*vmctx).tail_arg = std::ptr::null_mut();
                return error_poison_ptr();
            }

            let callee = (*vmctx).tail_callee;
            let arg = (*vmctx).tail_arg;

            // Clear tail fields immediately
            (*vmctx).tail_callee = std::ptr::null_mut();
            (*vmctx).tail_arg = std::ptr::null_mut();

            if callee.is_null() {
                // No pending tail call — shouldn't happen, propagate null
                return std::ptr::null_mut();
            }

            // Reset call depth so tail-recursive loops don't hit the limit
            machine_state(vmctx).reset_call_depth();

            // Read code pointer from closure
            let code_ptr = *(callee.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);

            // Call the closure: fn(vmctx, self, arg) -> result
            let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
                std::mem::transmute(code_ptr);
            let result = func(vmctx, callee, arg);

            if !result.is_null() {
                // Real return value — done
                return result;
            }

            // Result is null — check if another tail call was stored
            if (*vmctx).tail_callee.is_null() {
                // Null result with no pending tail call — propagate null (error)
                return std::ptr::null_mut();
            }

            // Another tail call pending — loop
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::errors::take_runtime_error;
    use super::*;
    use std::cell::Cell;

    extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

    thread_local! {
        static TEST_RESULT: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
    }

    // SAFETY: Test-only mock thunk entry. Returns a pre-set pointer from thread-local storage.
    unsafe extern "C" fn test_thunk_entry(_vmctx: *mut VMContext, _thunk: *mut u8) -> *mut u8 {
        TEST_RESULT.with(|r| r.get())
    }

    #[test]
    fn test_heap_force_thunk_unevaluated() {
        unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
                machine_state: std::ptr::null_mut(),
            };

            // 1. Allocate a Lit object for the result
            let mut lit_buf = [0u8; heap_layout::LIT_SIZE];
            let lit_ptr = lit_buf.as_mut_ptr();
            heap_layout::write_header(lit_ptr, layout::TAG_LIT, heap_layout::LIT_SIZE as u32);
            *(lit_ptr.add(layout::LIT_TAG_OFFSET as usize)) = 0; // Int#
            *(lit_ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = 42;

            // 2. Allocate a thunk object
            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u32);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_UNEVALUATED;

            TEST_RESULT.with(|r| r.set(lit_ptr));
            *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET as usize) as *mut usize) =
                test_thunk_entry as *const () as usize;

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, lit_ptr);
            assert_eq!(
                *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)),
                layout::THUNK_EVALUATED
            );
            assert_eq!(
                *(thunk_ptr.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *const *mut u8),
                lit_ptr
            );
        }
    }

    #[test]
    fn test_heap_force_thunk_evaluated() {
        unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
                machine_state: std::ptr::null_mut(),
            };

            // 1. Result: a real heap object (Lit) so the force loop can read its tag
            let mut lit_buf = [0u8; 32];
            let lit_ptr = lit_buf.as_mut_ptr();
            heap_layout::write_header(lit_ptr, layout::TAG_LIT, 32);

            // 2. Already evaluated thunk pointing to that Lit
            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u32);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_EVALUATED;
            *(thunk_ptr.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *mut *mut u8) = lit_ptr;

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, lit_ptr);
        }
    }

    #[test]
    fn test_heap_force_thunk_blackhole() {
        crate::machine_state::test_support::with_test_machine(|| unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
                machine_state: std::ptr::null_mut(),
            };

            // Blackholed thunk
            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u32);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_BLACKHOLE;

            let res = heap_force(&mut vmctx, thunk_ptr);
            // Result should be the poison object
            assert_eq!(res, error_poison_ptr());

            let err = take_runtime_error().expect("Should have flagged error");
            assert!(matches!(err, RuntimeError::BlackHole));
        });
    }

    #[test]
    fn test_heap_force_thunk_null_code_ptr() {
        crate::machine_state::test_support::with_test_machine(|| unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
                machine_state: std::ptr::null_mut(),
            };

            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u32);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_UNEVALUATED;
            *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET as usize) as *mut usize) = 0;

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, error_poison_ptr());
            let err = take_runtime_error().expect("Should have flagged error");
            assert!(matches!(err, RuntimeError::NullFunPtr));
        });
    }

    /// L1: a thunk entry can return null (App's `null_propagate_block` /
    /// `trampoline_resolve`'s defensive "shouldn't happen" paths) without
    /// `has_runtime_error()` being set. Memoizing that null as the thunk's
    /// indirection would leave a null pointer for a LATER force to
    /// dereference when it follows `THUNK_EVALUATED`'s indirection —
    /// segfault. Confirms both the first force (returns poison, records
    /// `BadPointer`, memoizes a non-null indirection) and a SECOND force on
    /// the same (now `THUNK_EVALUATED`) thunk (must follow the memoized
    /// indirection safely, not dereference null).
    #[test]
    fn test_heap_force_thunk_null_result_is_not_memoized_as_null() {
        crate::machine_state::test_support::with_test_machine(|| unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
                machine_state: std::ptr::null_mut(),
            };

            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u32);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_UNEVALUATED;
            TEST_RESULT.with(|r| r.set(std::ptr::null_mut())); // entry returns null
            *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET as usize) as *mut usize) =
                test_thunk_entry as *const () as usize;

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, error_poison_ptr());
            assert!(!res.is_null(), "must never return null itself");
            let err = take_runtime_error().expect("null thunk result must set an error");
            assert!(matches!(err, RuntimeError::BadPointer), "got {err:?}");
            assert_eq!(
                *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)),
                layout::THUNK_EVALUATED
            );
            let memoized =
                *(thunk_ptr.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *const *mut u8);
            assert!(
                !memoized.is_null(),
                "the memoized indirection must never be null"
            );

            // A LATER force on the same (now THUNK_EVALUATED) thunk follows
            // the memoized indirection — must not dereference a null pointer.
            let res2 = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res2, error_poison_ptr());
        });
    }

    #[test]
    fn test_heap_force_thunk_bad_state() {
        crate::machine_state::test_support::with_test_machine(|| unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
                machine_state: std::ptr::null_mut(),
            };

            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u32);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = 255; // Invalid state

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, error_poison_ptr());
            let err = take_runtime_error().expect("Should have flagged error");
            assert!(matches!(err, RuntimeError::BadThunkState(255)));
        });
    }

    /// M3: a large, ALREADY-EVALUATED (thunk-free) linear Con chain gives
    /// `heap_force` nothing to allocate for, so it never reaches a GC point —
    /// the only way `deep_force` can observe an external cancellation is the
    /// EXPLICIT periodic check added in this fix. Pre-set the cancel flag,
    /// then confirm a chain several `CANCEL_CHECK_INTERVAL`s long bails with
    /// `RuntimeError::Cancelled` instead of walking to the end.
    #[test]
    fn test_deep_force_observes_cancel_with_no_gc_points() {
        use crate::machine_state::{
            install_current_machine, restore_current_machine, MachineState,
        };
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        const N: usize = CANCEL_CHECK_INTERVAL as usize * 3;
        const CON_SIZE: usize = layout::CON_FIELDS_OFFSET as usize + 8; // 1 field
        let mut buf = vec![0u8; layout::LIT_TOTAL_SIZE as usize + N * CON_SIZE + 64];

        let ms = MachineState::new();
        // `check_cancel_and_set_error` records `RuntimeError::Cancelled` via
        // the CURRENT_MACHINE-reached free-fn shim (`errors::set_first_cause`),
        // NOT through `vmctx`, so this machine needs to be BOTH the vmctx's
        // machine AND the thread's CURRENT_MACHINE for the whole call.
        let prev_machine = install_current_machine(&ms as *const MachineState as *mut MachineState);
        let mut vmctx = VMContext {
            alloc_ptr: std::ptr::null_mut(),
            alloc_limit: std::ptr::null_mut(),
            gc_trigger: mock_gc_trigger,
            tail_callee: std::ptr::null_mut(),
            tail_arg: std::ptr::null_mut(),
            machine_state: &ms as *const MachineState as *mut MachineState,
        };
        let vmctx_ptr = &mut vmctx as *mut VMContext;

        unsafe {
            // Terminal Lit.
            let lit_ptr = buf.as_mut_ptr();
            heap_layout::write_header(lit_ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u32);
            *lit_ptr.add(layout::LIT_TAG_OFFSET as usize) = layout::LIT_TAG_INT as u8;
            *(lit_ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = 0;

            // N single-field Cons, each pointing to the previous (no sharing —
            // dedup would otherwise mask whether the cancel check itself works).
            let mut child = lit_ptr;
            let mut off = layout::LIT_TOTAL_SIZE as usize;
            for _ in 0..N {
                let here = buf.as_mut_ptr().add(off);
                heap_layout::write_header(here, layout::TAG_CON, CON_SIZE as u32);
                *(here.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = 1;
                *(here.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = 1;
                *(here.add(layout::CON_FIELDS_OFFSET as usize) as *mut *mut u8) = child;
                child = here;
                off += CON_SIZE;
            }
            let head = child;

            ms.set_cancel_flag(Arc::new(AtomicBool::new(true)));

            let result = deep_force(vmctx_ptr, head);
            assert_eq!(
                result,
                error_poison_ptr(),
                "a pre-set cancel flag must short-circuit deep_force, not run to completion"
            );
            let err = take_runtime_error().expect("cancel must set a runtime error");
            assert!(matches!(err, RuntimeError::Cancelled), "got {err:?}");
        }

        restore_current_machine(prev_machine);
    }
}
