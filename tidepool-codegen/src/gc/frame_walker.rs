use crate::stack_map::StackMapRegistry;

/// A collected GC root: the address on the stack where a heap pointer lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackRoot {
    /// Address on the stack containing the heap pointer.
    pub stack_slot_addr: *mut u64,
    /// Current value of the heap pointer.
    pub heap_ptr: *mut u8,
}

/// Why a frame walk could not prove a complete root snapshot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameWalkError {
    #[error("no stack-map registry is installed for collection")]
    RegistryUnavailable,
    #[error("frame address computation overflowed at {address:#x} ({operation})")]
    AddressOverflow {
        address: usize,
        operation: &'static str,
    },
    #[error(
        "{kind} address {address:#x} is outside stack bounds {low:#x}..{high:#x} or misaligned"
    )]
    InvalidAddress {
        kind: &'static str,
        address: usize,
        low: usize,
        high: usize,
    },
    #[error("caller frame {caller_fp:#x} is smaller than frame size {frame_size}")]
    FrameSizeUnderflow { caller_fp: usize, frame_size: u32 },
    #[error("safepoint stack pointer {sp:#x} is outside stack bounds {low:#x}..{high:#x}")]
    InvalidSafepointSp { sp: usize, low: usize, high: usize },
    #[error("JIT return address {return_addr:#x} has no stack-map entry")]
    MissingStackMap { return_addr: usize },
    #[error("invalid frame link {fp:#x} -> {saved_fp:#x}")]
    InvalidFrameLink { fp: usize, saved_fp: usize },
}

/// Conservative fallback span (bytes) for [`StackBounds::capture`]'s HIGH
/// bound when the platform thread-stack query is unavailable or fails.
/// Chosen larger than any `stack_size` this repo's test harnesses hand to a
/// JIT-driving thread (up to 256 MiB, `tests/call_depth_sequential_vs_nested.rs`),
/// so a correct frame chain is never rejected; a genuinely wild `fp` is still
/// caught because it must additionally fall within this span of the caller's
/// own stack address.
const CONSERVATIVE_STACK_SPAN: usize = 512 * 1024 * 1024;

/// Explicit bounds on the stack region `walk_frames` is allowed to read.
/// Every address the walker dereferences (a frame's saved-FP slot, its
/// return-address slot, and every stack-map root slot) must be proven to lie
/// in `[low, high)` — checked with saturating/checked arithmetic, never
/// assumed — before it is read.
#[derive(Debug, Clone, Copy)]
pub struct StackBounds {
    pub low: usize,
    pub high: usize,
}

impl StackBounds {
    /// Construct explicit bounds. `low <= high` is expected but not required
    /// by callers outside tests — a caller that gets this wrong just gets an
    /// always-empty walk (every `contains` check fails), not UB.
    pub fn new(low: usize, high: usize) -> Self {
        Self { low, high }
    }

    /// Bounds for the stack the CURRENT thread is running on, given `low`:
    /// the address of a local in a frame the caller knows sits below (at a
    /// lower address than) every JIT frame it wants to walk — e.g. a local
    /// in `perform_gc`, which is always called beneath the JIT call chain.
    /// `high` comes from a real thread-stack-top query when the platform
    /// supports one; otherwise `low + CONSERVATIVE_STACK_SPAN`, a bound this
    /// module can justify (see its doc) but did not measure.
    pub fn capture(low: usize) -> Self {
        let high = query_stack_top().unwrap_or_else(|| low.saturating_add(CONSERVATIVE_STACK_SPAN));
        Self::new(low, high.max(low))
    }

    /// Whether the `len`-byte range starting at `addr` lies entirely within
    /// `[low, high)`. Uses checked addition so an `addr` near `usize::MAX`
    /// (a corrupt frame pointer) reports out-of-bounds instead of wrapping.
    fn contains(&self, addr: usize, len: usize) -> bool {
        match addr.checked_add(len) {
            Some(end) => addr >= self.low && end <= self.high,
            None => false,
        }
    }
}

/// Real thread-stack-top query (the highest, i.e. numerically largest,
/// address in the calling thread's stack region — the stack grows down from
/// here). Returns `None` when unsupported or the underlying call fails, in
/// which case [`StackBounds::capture`] falls back to a conservative span.
#[cfg(target_os = "linux")]
fn query_stack_top() -> Option<usize> {
    // SAFETY: `attr` is initialized by `pthread_getattr_np` before any other
    // field is read, and destroyed exactly once on every return path.
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return None;
        }
        let mut stackaddr: *mut libc::c_void = std::ptr::null_mut();
        let mut stacksize: libc::size_t = 0;
        let rc = libc::pthread_attr_getstack(&attr, &mut stackaddr, &mut stacksize);
        libc::pthread_attr_destroy(&mut attr);
        if rc != 0 || stackaddr.is_null() {
            return None;
        }
        (stackaddr as usize).checked_add(stacksize)
    }
}

#[cfg(target_os = "macos")]
fn query_stack_top() -> Option<usize> {
    // SAFETY: `pthread_self` is always a valid handle for the calling thread.
    unsafe {
        let addr = libc::pthread_get_stackaddr_np(libc::pthread_self());
        if addr.is_null() {
            None
        } else {
            Some(addr as usize)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn query_stack_top() -> Option<usize> {
    None
}

/// Walk JIT frames starting from the given frame pointer, collecting all GC roots.
///
/// Uses Cranelift's `frame_size` metadata (the FP-to-SP distance, aka `active_size()`)
/// to compute SP at each safepoint: `SP = caller_FP - frame_size`. Correct on both
/// x86_64 and aarch64, regardless of prologue structure or callee-saved register layout.
///
/// # Safety
/// - `start_fp` must be a valid frame pointer from within a JIT call chain
///   (typically gc_trigger's FP, read via inline asm), OR any value at all —
///   an invalid `start_fp` is a controlled failure, not UB, PROVIDED `bounds`
///   correctly excludes it.
/// - `stack_maps` must contain entries for every live JIT safepoint. A return
///   address inside registered JIT code without an exact entry fails the walk.
/// - `bounds` must be a `StackBounds` the caller can justify contains every
///   frame it expects to walk (see [`StackBounds::capture`]).
///
/// # What is enforced
/// Every address this function dereferences — a frame's saved-FP slot, its
/// return-address slot, and every stack-map-derived root slot — is proven to
/// lie within `bounds` and to be 8-byte aligned before the read happens, and
/// every address computation (`fp + 8`, `caller_fp - frame_size`,
/// `sp_at_safepoint + offset`) uses checked arithmetic. A frame chain that
/// disagrees with the stack-map metadata it is checked against — a corrupt
/// saved FP, a `frame_size` that walks `sp_at_safepoint` outside `bounds`, or
/// a stack-map offset landing outside `bounds` — cannot produce a wild read
/// or write: it is caught before the dereference, an always-on `[BUG]`
/// breadcrumb is printed naming the violated condition, and the whole walk
/// fails. Partial roots are never returned as a collection-ready snapshot. Under
/// diagnostic mode (`diagnostic_mode = true`, wired to `TIDEPOOL_HEAP_VERIFY`
/// at the call site) the same condition panics instead, so a test can assert
/// on it deterministically.
///
/// A zero saved frame pointer is the explicit clean activation boundary.
/// Nonzero self/backward links and JIT PCs without exact safepoint metadata
/// are integrity failures, not alternate termination forms.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub unsafe fn walk_frames(
    start_fp: usize,
    stack_maps: &StackMapRegistry,
    bounds: StackBounds,
    diagnostic_mode: bool,
) -> Result<Vec<StackRoot>, FrameWalkError> {
    let mut roots = Vec::new();
    let mut fp = start_fp;

    let fail = |error: FrameWalkError| {
        eprintln!("[BUG] walk_frames: {error}");
        if diagnostic_mode {
            panic!("[BUG] walk_frames: {error}");
        }
        error
    };

    loop {
        if fp == 0 {
            break;
        }

        let Some(return_addr_slot) = fp.checked_add(8) else {
            return Err(fail(FrameWalkError::AddressOverflow {
                address: fp,
                operation: "fp + return-address offset",
            }));
        };

        if !bounds.contains(fp, 8) || !fp.is_multiple_of(8) {
            return Err(fail(FrameWalkError::InvalidAddress {
                kind: "frame pointer",
                address: fp,
                low: bounds.low,
                high: bounds.high,
            }));
        }
        if !bounds.contains(return_addr_slot, 8) || !return_addr_slot.is_multiple_of(8) {
            return Err(fail(FrameWalkError::InvalidAddress {
                kind: "return-address slot",
                address: return_addr_slot,
                low: bounds.low,
                high: bounds.high,
            }));
        }

        // SAFETY: both slots were proven in-bounds and aligned above.
        let return_addr = unsafe { *(return_addr_slot as *const usize) };
        let saved_fp = unsafe { *(fp as *const usize) };

        if !stack_maps.contains_address(return_addr) {
            // Native frames in a JIT -> host -> JIT sandwich carry no map.
            // A zero saved FP is the explicit clean activation boundary.
            if saved_fp == 0 {
                break;
            }
            if saved_fp <= fp {
                return Err(fail(FrameWalkError::InvalidFrameLink { fp, saved_fp }));
            }
            fp = saved_fp;
            continue;
        }

        // A PC inside registered JIT code must be an exact safepoint. Merely
        // belonging to the function range is not enough to trace its roots.
        let Some(info) = stack_maps.lookup(return_addr) else {
            return Err(fail(FrameWalkError::MissingStackMap { return_addr }));
        };
        let caller_fp = saved_fp;
        let Some(sp_at_safepoint) = caller_fp.checked_sub(info.frame_size as usize) else {
            return Err(fail(FrameWalkError::FrameSizeUnderflow {
                caller_fp,
                frame_size: info.frame_size,
            }));
        };
        if !bounds.contains(sp_at_safepoint, 0) {
            return Err(fail(FrameWalkError::InvalidSafepointSp {
                sp: sp_at_safepoint,
                low: bounds.low,
                high: bounds.high,
            }));
        }

        for &offset in &info.offsets {
            let Some(root_addr) = sp_at_safepoint.checked_add(offset as usize) else {
                return Err(fail(FrameWalkError::AddressOverflow {
                    address: sp_at_safepoint,
                    operation: "safepoint SP + stack-map offset",
                }));
            };
            if !bounds.contains(root_addr, 8) || !root_addr.is_multiple_of(8) {
                return Err(fail(FrameWalkError::InvalidAddress {
                    kind: "stack-map root slot",
                    address: root_addr,
                    low: bounds.low,
                    high: bounds.high,
                }));
            }
            // SAFETY: root_addr was proven in-bounds and aligned above.
            let heap_ptr = unsafe { *(root_addr as *const u64) as *mut u8 };
            roots.push(StackRoot {
                stack_slot_addr: root_addr as *mut u64,
                heap_ptr,
            });
        }

        if saved_fp == 0 {
            break;
        }
        if saved_fp <= fp {
            return Err(fail(FrameWalkError::InvalidFrameLink { fp, saved_fp }));
        }
        fp = saved_fp;
    }

    Ok(roots)
}
