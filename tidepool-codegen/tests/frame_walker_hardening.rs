//! Direct unit coverage for `gc::frame_walker::walk_frames` against a
//! SYNTHETIC stack buffer (plain heap memory laid out to look like a chain of
//! saved-FP/return-address frames) and a real `StackMapRegistry`. No JIT
//! compile involved — this drives the real walker and the real registry type
//! against deliberately well-formed and deliberately corrupt metadata.
//!
//! Every non-terminating chain here is built inside one `Vec<u64>` (8-aligned
//! by construction), so `SyntheticStack::bounds()` is a `StackBounds` the
//! test can fully justify: nothing outside that one buffer is ever a valid
//! address in these tests.

use cranelift_codegen::ir::types;
use tidepool_codegen::gc::frame_walker::{walk_frames, StackBounds, StackRoot};
use tidepool_codegen::stack_map::{RawStackMap, RawStackMapEntry, StackMapRegistry};

/// A fake JIT code region: never executed, never dereferenced — only its
/// address range and the registry entries keyed against it matter.
const JIT_BASE: usize = 0x1_0000_0000;
const JIT_CODE_OFFSET: u32 = 0x40;
const JIT_RETURN_ADDR: usize = JIT_BASE + JIT_CODE_OFFSET as usize;

/// An address that is guaranteed not to fall inside `JIT_BASE`'s registered
/// range — used as the return address of a non-JIT (host) frame.
const NON_JIT_RETURN_ADDR: usize = 0xDEAD_0000;

/// A `StackMapRegistry` with one registered safepoint at `JIT_RETURN_ADDR`,
/// with the given `frame_size`/`offsets`.
fn jit_registry(frame_size: u32, offsets: &[u32]) -> StackMapRegistry {
    let mut reg = StackMapRegistry::new();
    let entries = vec![RawStackMap {
        code_offset: JIT_CODE_OFFSET,
        frame_size,
        entries: offsets
            .iter()
            .map(|&offset| RawStackMapEntry {
                ty: types::I64,
                offset,
            })
            .collect(),
    }];
    reg.register(JIT_BASE, JIT_CODE_OFFSET + 0x10, &entries);
    reg
}

/// Plain heap memory laid out as a synthetic frame chain: `[u64; N]`,
/// 8-aligned by construction (backed by `Vec<u64>`), addressed by word index.
struct SyntheticStack {
    words: Vec<u64>,
}

impl SyntheticStack {
    fn new(word_count: usize) -> Self {
        Self {
            words: vec![0u64; word_count],
        }
    }

    fn base(&self) -> usize {
        self.words.as_ptr() as usize
    }

    fn end(&self) -> usize {
        self.base() + self.words.len() * 8
    }

    /// Bounds covering exactly this buffer — the test's justification is
    /// that every address it treats as valid lives inside this one `Vec`.
    fn bounds(&self) -> StackBounds {
        StackBounds::new(self.base(), self.end())
    }

    fn addr(&self, word_idx: usize) -> usize {
        self.base() + word_idx * 8
    }

    fn set(&mut self, word_idx: usize, value: u64) {
        self.words[word_idx] = value;
    }
}

/// (a) POSITIVE CONTROL: a well-formed synthetic chain — one JIT frame with
/// two stack-map roots, followed by a terminating non-JIT frame — collects
/// exactly the expected roots. Without this passing, the negative cases below
/// prove nothing (a walker that always returns empty would "pass" them too).
#[test]
fn well_formed_chain_collects_expected_roots() {
    let mut stack = SyntheticStack::new(16);

    // Frame 0: JIT frame. Saved FP -> frame 1 (word 8); return address is
    // the registered JIT safepoint.
    stack.set(0, stack.addr(8) as u64);
    stack.set(1, JIT_RETURN_ADDR as u64);
    // Root payload the stack map points at: sp_at_safepoint = caller_fp(word
    // 8) - frame_size(48) = addr(8) - 48 = addr(2). offsets [0, 8] land on
    // words 2 and 3.
    stack.set(2, 0x1111_1111_1111_1111);
    stack.set(3, 0x2222_2222_2222_2222);

    // Frame 1: terminating non-JIT frame (e.g. the gc_trigger -> perform_gc
    // sandwich). Saved FP = 0 stops the walk cleanly.
    stack.set(8, 0);
    stack.set(9, NON_JIT_RETURN_ADDR as u64);

    let registry = jit_registry(48, &[0, 8]);
    let bounds = stack.bounds();
    let start_fp = stack.addr(0);

    let roots = unsafe { walk_frames(start_fp, &registry, bounds, false) };

    let expected = vec![
        StackRoot {
            stack_slot_addr: stack.addr(2) as *mut u64,
            heap_ptr: 0x1111_1111_1111_1111u64 as *mut u8,
        },
        StackRoot {
            stack_slot_addr: stack.addr(3) as *mut u64,
            heap_ptr: 0x2222_2222_2222_2222u64 as *mut u8,
        },
    ];
    assert_eq!(roots, expected);
}

/// (b) `frame_size` larger than the actual frame: `sp_at_safepoint` lands
/// outside the supplied bounds even though the subtraction itself doesn't
/// underflow. Controlled failure — empty result, no panic — not a wild read.
#[test]
fn oversized_frame_size_is_controlled_failure() {
    let mut stack = SyntheticStack::new(4);
    stack.set(0, stack.addr(2) as u64);
    stack.set(1, JIT_RETURN_ADDR as u64);

    // frame_size far larger than the 4-word buffer: sp_at_safepoint
    // undershoots `bounds.low` by a huge margin.
    let registry = jit_registry(10_000, &[0]);
    let bounds = stack.bounds();
    let start_fp = stack.addr(0);

    let roots = unsafe { walk_frames(start_fp, &registry, bounds, false) };
    assert!(
        roots.is_empty(),
        "oversized frame_size must not yield any roots, got {roots:?}"
    );
}

/// Same corruption as above, but under diagnostic mode (the
/// `TIDEPOOL_HEAP_VERIFY` path): the violation panics instead of returning,
/// so a test (or a real run) can assert on it deterministically.
#[test]
fn oversized_frame_size_panics_in_diagnostic_mode() {
    let mut stack = SyntheticStack::new(4);
    stack.set(0, stack.addr(2) as u64);
    stack.set(1, JIT_RETURN_ADDR as u64);

    let registry = jit_registry(10_000, &[0]);
    let bounds = stack.bounds();
    let start_fp = stack.addr(0);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        walk_frames(start_fp, &registry, bounds, true)
    }));
    assert!(
        result.is_err(),
        "diagnostic mode must panic on an out-of-bounds sp_at_safepoint"
    );
}

/// (c) A stack-map offset that lands outside bounds even though
/// `sp_at_safepoint` itself is valid: controlled failure, and roots
/// collected before the bad offset (within the same frame) are kept.
#[test]
fn out_of_bounds_stack_map_offset_is_controlled_failure() {
    let mut stack = SyntheticStack::new(4);
    // sp_at_safepoint = caller_fp(word 3) - frame_size(8) = addr(3) - 8 = addr(2).
    stack.set(0, stack.addr(3) as u64);
    stack.set(1, JIT_RETURN_ADDR as u64);
    stack.set(2, 0x3333_3333_3333_3333);

    // First offset (0) is valid; second (way past the buffer) is not.
    let registry = jit_registry(8, &[0, 1_000_000]);
    let bounds = stack.bounds();
    let start_fp = stack.addr(0);

    let roots = unsafe { walk_frames(start_fp, &registry, bounds, false) };
    assert_eq!(
        roots,
        vec![StackRoot {
            stack_slot_addr: stack.addr(2) as *mut u64,
            heap_ptr: 0x3333_3333_3333_3333u64 as *mut u8,
        }],
        "only the root collected before the bad offset should survive"
    );
}

/// Same corruption as above under diagnostic mode: panics.
#[test]
fn out_of_bounds_stack_map_offset_panics_in_diagnostic_mode() {
    let mut stack = SyntheticStack::new(4);
    stack.set(0, stack.addr(3) as u64);
    stack.set(1, JIT_RETURN_ADDR as u64);
    stack.set(2, 0x3333_3333_3333_3333);

    let registry = jit_registry(8, &[0, 1_000_000]);
    let bounds = stack.bounds();
    let start_fp = stack.addr(0);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        walk_frames(start_fp, &registry, bounds, true)
    }));
    assert!(
        result.is_err(),
        "diagnostic mode must panic on an out-of-bounds stack-map offset"
    );
}

/// (d) A frame pointer outside the supplied bounds stops the walk
/// immediately — no dereference of `start_fp` or `start_fp+8` is attempted.
#[test]
fn fp_outside_bounds_stops_immediately() {
    let stack = SyntheticStack::new(2);
    let bounds = stack.bounds();
    // Nowhere near the buffer; never valid to dereference. If walk_frames
    // dereferenced it before checking bounds, this test would segfault
    // instead of returning.
    let start_fp = bounds.high + 0x1000_0000;

    let registry = jit_registry(0, &[]);
    let roots = unsafe { walk_frames(start_fp, &registry, bounds, false) };
    assert!(roots.is_empty());
}

/// Same corruption as above under diagnostic mode: panics.
#[test]
fn fp_outside_bounds_panics_in_diagnostic_mode() {
    let stack = SyntheticStack::new(2);
    let bounds = stack.bounds();
    let start_fp = bounds.high + 0x1000_0000;

    let registry = jit_registry(0, &[]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        walk_frames(start_fp, &registry, bounds, true)
    }));
    assert!(
        result.is_err(),
        "diagnostic mode must panic on a frame pointer outside bounds"
    );
}

/// (e) A terminating non-JIT frame — return address not in the registry,
/// saved FP is 0 — is skipped and the walk terminates cleanly with no roots
/// and no failure, whether or not diagnostic mode is on.
#[test]
fn terminating_non_jit_frame_stops_cleanly() {
    let mut stack = SyntheticStack::new(2);
    stack.set(0, 0); // saved FP = 0: terminate
    stack.set(1, NON_JIT_RETURN_ADDR as u64);

    let bounds = stack.bounds();
    let start_fp = stack.addr(0);
    let registry = jit_registry(0, &[]); // no safepoint at NON_JIT_RETURN_ADDR

    let roots = unsafe { walk_frames(start_fp, &registry, bounds, false) };
    assert!(roots.is_empty());

    let roots_diagnostic = unsafe { walk_frames(start_fp, &registry, bounds, true) };
    assert!(
        roots_diagnostic.is_empty(),
        "a clean terminating non-JIT frame must not panic even in diagnostic mode"
    );
}
