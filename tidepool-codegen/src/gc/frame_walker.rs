use crate::stack_map::StackMapRegistry;

/// A collected GC root: the address on the stack where a heap pointer lives.
#[derive(Debug, Clone, Copy)]
pub struct StackRoot {
    /// Address on the stack containing the heap pointer.
    pub stack_slot_addr: *mut u64,
    /// Current value of the heap pointer.
    pub heap_ptr: *mut u8,
}

/// Walk JIT frames starting from the given RBP, collecting all GC roots.
///
/// # Safety
/// - `start_rbp` must be a valid frame pointer from within a JIT call chain.
/// - `stack_maps` must contain entries for all JIT functions in the call chain.
#[cfg(target_arch = "x86_64")]
pub unsafe fn walk_frames(
    start_rbp: usize,
    stack_maps: &StackMapRegistry,
    start_rsp: usize,
) -> Vec<StackRoot> {
    let mut roots = Vec::new();
    let mut rbp = start_rbp;
    
    // First, try to find the first JIT return address by searching up from RSP.
    // This handles cases where gc_trigger doesn't have a frame pointer.
    let mut current_return_addr = None;
    let mut search_ptr = start_rsp;
    // We search up to RBP + 16 (where the JIT return addr would be if gc_trigger has no frame).
    while search_ptr < start_rbp + 16 {
        let val = *(search_ptr as *const usize);
        if stack_maps.contains_address(val) {
            current_return_addr = Some(val);
            // If we found a JIT return address, the RBP for this frame is the current RBP
            // if gc_trigger has no frame, or it's the saved RBP if it does.
            // Actually, if we found a JIT return address at search_ptr, 
            // then search_ptr + 8 is the SP at the safepoint!
            break;
        }
        search_ptr += 8;
    }

    loop {
        if rbp == 0 {
            break;
        }

        // Determine return address for this frame
        let return_addr = if let Some(addr) = current_return_addr.take() {
            addr
        } else {
            *((rbp + 8) as *const usize)
        };
        
        // Check if this return address is in JIT code
        if !stack_maps.contains_address(return_addr) {
            if !roots.is_empty() {
                // We were in JIT territory and now we left it. Stop.
                break;
            } else {
                // We haven't hit JIT territory yet. Skip this frame.
                let next_rbp = *(rbp as *const usize);
                if next_rbp == 0 || next_rbp == rbp || next_rbp < rbp {
                    break;
                }
                rbp = next_rbp;
                continue;
            }
        }

        // Look up stack map for this return address
        if let Some(info) = stack_maps.lookup(return_addr) {
            // Compute SP at safepoint.
            // Cranelift stack map offsets are SP-relative at the safepoint.
            // The return address we found is the one pushed by the 'call' in JIT code.
            // The SP just before that 'call' was (addr_of_return_addr + 8).
            
            // We need to find where this return_addr was on the stack.
            //
            // If this is the first frame we found, and it was found via the initial RSP search
            // (proxied by `search_ptr < start_rbp + 16`), we use that search address.
            // Otherwise, for any subsequent JIT frames, the return address is at [rbp + 8].
            let addr_of_return_addr = if roots.is_empty() && search_ptr < start_rbp + 16 {
                search_ptr
            } else {
                rbp + 8
            };
            
            let sp_at_safepoint = addr_of_return_addr + 8;

            for &offset in &info.offsets {
                let root_addr = (sp_at_safepoint + offset as usize) as *mut u64;
                let heap_ptr = *root_addr as *mut u8;
                roots.push(StackRoot {
                    stack_slot_addr: root_addr,
                    heap_ptr,
                });
            }
        }

        // Walk to next frame: *(rbp) is the saved caller RBP
        let next_rbp = *(rbp as *const usize);
        
        // Basic sanity checks to prevent infinite loops or jumping to null
        if next_rbp == 0 || next_rbp == rbp || next_rbp < rbp {
            break;
        }
        rbp = next_rbp;
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
