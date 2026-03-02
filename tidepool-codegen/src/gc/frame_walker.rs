use crate::stack_map::StackMapRegistry;

/// A collected GC root: the address on the stack where a heap pointer lives.
#[derive(Debug, Clone, Copy)]
pub struct StackRoot {
    /// Address on the stack containing the heap pointer.
    pub stack_slot_addr: *mut u64,
    /// Current value of the heap pointer.
    pub heap_ptr: *mut u8,
}

/// Walk JIT frames starting from the given frame pointer, collecting all GC roots.
///
/// Uses Cranelift's `frame_size` metadata (the FP-to-SP distance, aka `active_size()`)
/// to compute SP at each safepoint: `SP = caller_FP - frame_size`. This is the same
/// approach Wasmtime uses and is correct on both x86_64 and aarch64, regardless of
/// prologue structure or callee-saved register layout.
///
/// # Safety
/// - `start_fp` must be a valid frame pointer from within a JIT call chain
///   (typically gc_trigger's FP, read via inline asm).
/// - `stack_maps` must contain entries for all JIT functions in the call chain.
/// - All frames in the chain must have frame pointers (`force-frame-pointers = true`).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub unsafe fn walk_frames(start_fp: usize, stack_maps: &StackMapRegistry) -> Vec<StackRoot> {
    let mut roots = Vec::new();
    let mut fp = start_fp;

    loop {
        if fp == 0 {
            break;
        }

        // [FP+8] = return address (into the caller of this frame's function)
        let return_addr = *((fp + 8) as *const usize);

        // Check if this return address is in JIT code
        if !stack_maps.contains_address(return_addr) {
            if !roots.is_empty() {
                // We were in JIT territory and now we left it. Stop.
                break;
            }
            // We haven't hit JIT territory yet. Skip this frame
            // (e.g., gc_trigger → perform_gc before reaching JIT frames).
            let next_fp = *(fp as *const usize);
            if next_fp == 0 || next_fp == fp || next_fp <= fp {
                break;
            }
            fp = next_fp;
            continue;
        }

        // Found a JIT return address. The stack map at this address describes
        // the caller's GC roots at the point it made the call.
        if let Some(info) = stack_maps.lookup(return_addr) {
            // The caller's FP is saved at [current_FP + 0].
            let caller_fp = *(fp as *const usize);
            // SP at the safepoint = caller's FP - caller's active frame size.
            // Cranelift's frame_size is active_size(): the distance from FP down to SP.
            let sp_at_safepoint = caller_fp - info.frame_size as usize;

            for &offset in &info.offsets {
                let root_addr = (sp_at_safepoint + offset as usize) as *mut u64;
                let heap_ptr = *root_addr as *mut u8;
                roots.push(StackRoot {
                    stack_slot_addr: root_addr,
                    heap_ptr,
                });
            }
        }

        // Walk to next frame: [FP+0] is the saved caller FP
        let next_fp = *(fp as *const usize);

        // Sanity checks to prevent infinite loops
        if next_fp == 0 || next_fp == fp || next_fp <= fp {
            break;
        }
        fp = next_fp;
    }

    roots
}

/// Rewrite forwarding pointers in stack slots after GC.
///
/// For each root, if the heap object has been moved (forwarding pointer),
/// update the stack slot to point to the new location.
///
/// # Safety
/// All roots must still be valid stack addresses.
pub unsafe fn rewrite_roots(roots: &[StackRoot], forwarding_map: &dyn Fn(*mut u8) -> *mut u8) {
    for root in roots {
        let new_ptr = forwarding_map(root.heap_ptr);
        if new_ptr != root.heap_ptr {
            *root.stack_slot_addr = new_ptr as u64;
        }
    }
}
