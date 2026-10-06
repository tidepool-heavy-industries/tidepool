use crate::stack_map::StackMapLookup;

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
    #[error("no current-thread native stack mapping is admitted")]
    StackMappingUnavailable,
    #[error("native stack admission belongs to another thread")]
    StackThreadMismatch,
    #[error("current stack address {address:#x} is outside admitted mapping {low:#x}..{high:#x}")]
    InvalidStackMapping {
        address: usize,
        low: usize,
        high: usize,
    },
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
/// - `stack_maps` is a chain, tried in order for every frame: the union of
///   every registered JIT pipeline's registry (one per installed program on
///   a `PreparedMachine`). It must contain entries for every live JIT
///   safepoint across every pipeline that may appear in this call chain.
///   Return addresses never collide across pipelines, so at most one
///   registry in the chain recognizes any given frame; a return address
///   inside registered JIT code without an exact entry in the recognizing
///   registry fails the walk. An unregistered pipeline's frame is skipped
///   as if it were a host frame, so soundness rests on the chain being
///   complete, which install/rollback (see `PreparedMachine::install`'s T3
///   acceptance) guarantees.
/// - `bounds` must be a `StackBounds` the caller can justify contains every
///   frame it expects to walk and lies in actual readable storage. Production
///   bounds come only from the active current-thread native admission.
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
pub unsafe fn walk_frames<L: StackMapLookup + ?Sized>(
    start_fp: usize,
    stack_maps: &L,
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

        // Return addresses never collide across pipelines, so at most one
        // registry recognizes this frame.
        let Some(owning_registry) = stack_maps.registry_for(return_addr) else {
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
        };

        // A PC inside registered JIT code must be an exact safepoint. Merely
        // belonging to the function range is not enough to trace its roots.
        let Some(info) = owning_registry.lookup(return_addr) else {
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
