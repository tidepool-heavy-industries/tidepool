//! Stateful op-sequence proptests for the ByteArray, boxed-array, and Double
//! host functions in `tidepool_codegen::host_fns` (W7 host-arrays).
//!
//! These functions are virgin territory: only the TEXT host fns had proptests.
//! Each one performs raw pointer arithmetic through `unsafe` accessors, so a
//! fencepost error is a SIGSEGV, not a wrong answer. This suite hunts both.
//!
//! # Driver route (documented per the task boundary)
//!
//! **Direct extern-C host-fn calls**, matching the existing
//! `proptest_host_fns.rs` precedent — NOT hand-built CoreExpr PrimOp trees.
//!
//! The PrimOps *do* exist (`NewByteArray`, `CopyByteArray`, `ShrinkMutableByteArray`,
//! `ResizeMutableByteArray`, `CompareByteArrays`, `NewArray`, `CloneArray`, …),
//! but driving them through a CoreExpr requires threading `State# RealWorld`
//! tokens, materialising boxed `MutableByteArray#` Lit values, and decoding
//! unboxed result tuples — genuinely impractical and itself a source of test
//! bugs. Direct calls give exact control over offsets, lengths, fenceposts, and
//! overlapping ranges, which is the entire point of this suite.
//!
//! # Memory model
//!
//! `runtime_new_byte_array` / `runtime_new_boxed_array` allocate with
//! `std::alloc` (a `[u64 len][payload…]` buffer), NOT in the GC nursery — there
//! are zero references to these buffers in `gc.rs`. The model mirrors each
//! buffer with a `Vec<u8>` (bytes) or `Vec<i64>` (boxed slots; the stored words
//! are opaque tokens the host never dereferences).
//!
//! # B4 / GcPoint oracle (substitution, documented)
//!
//! Because these buffers are not GC-managed, a nursery GC physically cannot
//! relocate a ByteArray mid-sequence — the "tiny-nursery 4KB A/B" oracle is
//! N/A for the direct-call route. It is replaced by an equivalent that targets
//! the *real* bug class for malloc'd buffers: **run-the-same-sequence-twice
//! determinism** with `GcPoint` allocator-churn interleaved. This catches
//! use-after-free / dealloc bugs (`resize` frees the old buffer), allocator
//! reuse, and uninitialised-memory reads (e.g. a `resize` that failed to zero
//! the grown tail would hand back nondeterministic allocator bytes — divergent
//! across runs). `resize` currently uses `alloc_zeroed`, so this oracle should
//! pass; it exists to catch a regression to `alloc`.
//!
//! # Fork everything (B3)
//!
//! Every executing case runs in a `libc::fork` child (the unsafe accessor work
//! happens on an 8 MB child thread). A fatal signal (SIGSEGV/SIGILL/SIGBUS)
//! kills the child; the parent's `waitpid` sees `WIFSIGNALED` and converts it
//! into a shrinkable proptest failure (B3). Logical divergences (model mismatch
//! B1/B5, unexpected error on a valid sequence B2, run-twice divergence B4) are
//! reported by the child over a pipe and likewise shrink.
//!
//! NOT bugs: clean (flag-only) errors on documented-invalid inputs. The
//! generators only ever build VALID sequences (size ≥ 0, in-bounds-or-silently-
//! ignored offsets), so a set runtime-error flag on such a sequence IS a bug
//! (B2). A crash on any input is always a bug (B3).

#![allow(clippy::too_many_arguments)]

use proptest::prelude::*;
use serial_test::serial;
use std::alloc::{dealloc, Layout};
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use tidepool_codegen::host_fns::*;

// ---------------------------------------------------------------------------
// Fork-per-case harness (B3). libc resolves in integration tests (see
// signal_safety.rs). No Cargo.toml edit — raw `libc::` use only.
// ---------------------------------------------------------------------------

/// Outcome of running one case in a forked child.
enum Outcome {
    /// Child exited 0 and reported success over the pipe.
    Pass,
    /// Child reported a logical failure (B1/B2/B4/B5) — the string is the
    /// diagnostic written before a non-zero exit.
    Logical(String),
    /// Child died from a fatal signal (B3). Carries the signal number.
    Signal(i32),
}

/// Run `f` in a forked child, on an 8 MB-stack thread (hygiene: deep Value
/// spines / recursion get headroom even though this suite's work is iterative).
/// A fatal signal in the child is observed by the parent as `WIFSIGNALED`.
///
/// The child writes `[tag byte][utf-8 message…]` to a pipe then `_exit`s:
///   tag 0 = pass, tag 1 = logical failure, tag 2 = panic.
fn run_forked<F>(f: F) -> Outcome
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    let mut fds = [0i32; 2];
    // SAFETY: pipe() fills a 2-element i32 array with [read, write] fds.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Outcome::Logical("pipe() failed".to_string());
    }
    let (rd, wr) = (fds[0], fds[1]);

    // SAFETY: fork() duplicates the process. We are #[serial] so no other test
    // thread is allocating at the fork point (glibc malloc fork-safety).
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
        return Outcome::Logical("fork() failed".to_string());
    }

    if pid == 0 {
        // ---- child ----
        unsafe { libc::close(rd) };
        // Run the unsafe work on a generous 8 MB stack. A SIGSEGV here kills
        // the whole child process → parent sees WIFSIGNALED (that is the B3
        // signal we want to catch, not recover from).
        let joined = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(f)
            .map(|h| h.join());
        let (tag, msg): (u8, String) = match joined {
            Ok(Ok(Ok(()))) => (0, String::new()),
            Ok(Ok(Err(m))) => (1, m),
            Ok(Err(_)) => (2, "panic in child worker thread".to_string()),
            Err(_) => (2, "failed to spawn child worker thread".to_string()),
        };
        let mut buf = Vec::with_capacity(1 + msg.len());
        buf.push(tag);
        buf.extend_from_slice(msg.as_bytes());
        // SAFETY: writing our own buffer to the pipe write end, then closing.
        unsafe {
            let mut off = 0usize;
            while off < buf.len() {
                let n = libc::write(wr, buf.as_ptr().add(off) as *const c_void, buf.len() - off);
                if n <= 0 {
                    break;
                }
                off += n as usize;
            }
            libc::close(wr);
            // _exit, not exit: skip atexit/flush in the forked child.
            libc::_exit(if tag == 0 { 0 } else { 3 });
        }
    }

    // ---- parent ----
    unsafe { libc::close(wr) };
    let mut data = Vec::new();
    let mut tmp = [0u8; 512];
    loop {
        // SAFETY: reading the pipe read end into a stack buffer.
        let n = unsafe { libc::read(rd, tmp.as_mut_ptr() as *mut c_void, tmp.len()) };
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&tmp[..n as usize]);
        if data.len() > 8192 {
            break;
        }
    }
    unsafe { libc::close(rd) };

    let mut status = 0i32;
    // SAFETY: waitpid on our own child.
    unsafe { libc::waitpid(pid, &mut status as *mut i32, 0) };

    if libc::WIFSIGNALED(status) {
        return Outcome::Signal(libc::WTERMSIG(status));
    }
    match data.first().copied() {
        Some(0) => Outcome::Pass,
        Some(_) => Outcome::Logical(String::from_utf8_lossy(&data[1..]).into_owned()),
        // Exited (not signalled) but wrote nothing — treat as failure so it
        // cannot silently pass.
        None => Outcome::Logical("child exited without reporting (no pipe data)".to_string()),
    }
}

/// Translate an `Outcome` into a proptest assertion. Returns `Ok` on pass,
/// `Err(TestCaseError)` (which proptest shrinks) otherwise.
macro_rules! assert_outcome {
    ($outcome:expr, $ctx:expr) => {{
        match $outcome {
            Outcome::Pass => {}
            Outcome::Logical(m) => {
                prop_assert!(false, "logical divergence: {}\ncontext: {:?}", m, $ctx)
            }
            Outcome::Signal(s) => prop_assert!(
                false,
                "FATAL SIGNAL {} (B3 — crash in unsafe accessor)\ncontext: {:?}",
                s,
                $ctx
            ),
        }
    }};
}

// ---------------------------------------------------------------------------
// Fencepost sizes: alignment + word fenceposts (8-byte words), plus the
// 4 KiB page boundary. The strategies bias heavily toward these.
// ---------------------------------------------------------------------------

const FENCEPOSTS: [usize; 9] = [0, 1, 7, 8, 63, 64, 65, 4095, 4096];
const OFF_FENCEPOSTS: [usize; 9] = [0, 1, 7, 8, 63, 64, 65, 255, 256];

fn size_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![
        8 => prop::sample::select(FENCEPOSTS.to_vec()),
        2 => 0usize..256,
        1 => 0usize..4200,
    ]
}

fn off_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![
        3 => prop::sample::select(OFF_FENCEPOSTS.to_vec()),
        2 => 0usize..200,
        1 => 0usize..4200,
    ]
}

fn len_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![
        3 => prop::sample::select(OFF_FENCEPOSTS.to_vec()),
        2 => 0usize..200,
        1 => 0usize..4200,
    ]
}

// ---------------------------------------------------------------------------
// ByteArray op-sequence model
// ---------------------------------------------------------------------------

/// Which byte to target for a `Set`.
#[derive(Clone, Debug)]
enum IdxKind {
    First,
    Last,
    At(usize),
}

impl IdxKind {
    fn resolve(&self, size: usize) -> usize {
        match self {
            IdxKind::First => 0,
            IdxKind::Last => size.saturating_sub(1),
            IdxKind::At(x) => {
                if size == 0 {
                    0
                } else {
                    x % size
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
enum ArrOp {
    New {
        size: usize,
    },
    Set {
        slot: usize,
        idx: IdxKind,
        len: usize,
        val: u8,
    },
    /// Copy `len` bytes src[src_off..] -> dst[dst_off..]. When `same`, src and
    /// dst are the SAME array → overlapping memmove territory.
    Copy {
        same: bool,
        src_slot: usize,
        dst_slot: usize,
        src_off: usize,
        dst_off: usize,
        len: usize,
    },
    ShrinkTo {
        slot: usize,
        new: usize,
    },
    ResizeTo {
        slot: usize,
        new: usize,
    },
    Compare {
        a_slot: usize,
        b_slot: usize,
        a_off: usize,
        b_off: usize,
        len: usize,
    },
    /// Junk allocation churn to perturb the allocator between ops.
    GcPoint,
}

fn idx_kind_strategy() -> impl Strategy<Value = IdxKind> {
    prop_oneof![
        2 => Just(IdxKind::First),
        2 => Just(IdxKind::Last),
        3 => (0usize..100_000).prop_map(IdxKind::At),
    ]
}

fn arrop_strategy() -> impl Strategy<Value = ArrOp> {
    prop_oneof![
        3 => size_strategy().prop_map(|size| ArrOp::New { size }),
        4 => (0usize..8, idx_kind_strategy(), len_strategy(), any::<u8>())
            .prop_map(|(slot, idx, len, val)| ArrOp::Set { slot, idx, len, val }),
        5 => (any::<bool>(), 0usize..8, 0usize..8, off_strategy(), off_strategy(), len_strategy())
            .prop_map(|(same, src_slot, dst_slot, src_off, dst_off, len)| ArrOp::Copy {
                same, src_slot, dst_slot, src_off, dst_off, len
            }),
        2 => (0usize..8, size_strategy()).prop_map(|(slot, new)| ArrOp::ShrinkTo { slot, new }),
        2 => (0usize..8, size_strategy()).prop_map(|(slot, new)| ArrOp::ResizeTo { slot, new }),
        3 => (0usize..8, 0usize..8, off_strategy(), off_strategy(), len_strategy())
            .prop_map(|(a_slot, b_slot, a_off, b_off, len)| ArrOp::Compare {
                a_slot, b_slot, a_off, b_off, len
            }),
        1 => Just(ArrOp::GcPoint),
    ]
}

/// A live byte array: raw pointer + the backing allocation size (needed to
/// `dealloc` correctly — `shrink` updates only the logical length prefix, so
/// the backing size and the logical length diverge).
#[derive(Clone, Copy)]
struct RealBa {
    ptr: i64,
    backing: usize,
}

/// SAFETY: `ptr` points at a `[u64 len][bytes…]` buffer; read the length prefix.
unsafe fn ba_len(ptr: i64) -> usize {
    *(ptr as *const u64) as usize
}

/// SAFETY: `ptr` is a valid byte array; copy out its current logical bytes.
unsafe fn ba_bytes(ptr: i64) -> Vec<u8> {
    let n = ba_len(ptr);
    std::slice::from_raw_parts((ptr as *const u8).add(8), n).to_vec()
}

fn free_ba(b: RealBa) {
    // BUG-2 FIXED 2026-06-10: runtime_new/resize now allocate with a hidden
    // capacity word at ptr - 8 recording the TRUE allocation size, so the
    // dealloc layout comes from the allocation itself (immune to logical
    // shrinks). `b.backing` is kept for model assertions only.
    // SAFETY: ptr was produced by runtime_new/resize_byte_array; the
    // allocation base and total size live one word below it.
    unsafe {
        let base = (b.ptr as *mut u8).sub(8);
        let total = *(base as *const u64) as usize;
        let layout = Layout::from_size_align(total, 8).unwrap();
        dealloc(base, layout);
    }
}

/// Ranges [a, a+len) and [b, b+len) intersect (len > 0).
fn ranges_overlap(a: usize, b: usize, len: usize) -> bool {
    len > 0 && a < b + len && b < a + len
}

/// Run the op sequence against the real host fns AND the `Vec<u8>` model in
/// lockstep, asserting full-state equivalence after every op (stronger than
/// the required "after every Compare + at end"), error-flag cleanliness (B2),
/// and Compare-result equivalence (B1). Frees all live arrays before
/// returning. Returns a FNV-1a hash of the final model state (== real state)
/// for the run-twice (B4) oracle.
fn interp_bytearray(ops: &[ArrOp]) -> Result<u64, String> {
    let _ = take_runtime_error(); // clear any stale flag from a previous case
    let mut real: Vec<RealBa> = Vec::new();
    let mut model: Vec<Vec<u8>> = Vec::new();

    let result = (|| -> Result<u64, String> {
        for (i, op) in ops.iter().enumerate() {
            apply_ba_op(op, &mut real, &mut model).map_err(|m| format!("op#{i} {op:?}: {m}"))?;

            if let Some(e) = take_runtime_error() {
                return Err(format!(
                    "op#{i} {op:?} set runtime error {e:?} on a valid sequence (B2)"
                ));
            }
            check_ba_state(&real, &model).map_err(|m| format!("after op#{i} {op:?}: {m}"))?;
        }
        // FNV-1a over the final model state.
        let mut hash = 0xcbf29ce484222325u64;
        for a in &model {
            hash = hash.wrapping_mul(0x100000001b3) ^ (a.len() as u64);
            for &b in a {
                hash = hash.wrapping_mul(0x100000001b3) ^ (b as u64);
            }
        }
        Ok(hash)
    })();

    for &b in &real {
        free_ba(b);
    }
    result
}

/// Verify every real buffer matches its model twin.
fn check_ba_state(real: &[RealBa], model: &[Vec<u8>]) -> Result<(), String> {
    if real.len() != model.len() {
        return Err(format!(
            "slot count mismatch: real={} model={}",
            real.len(),
            model.len()
        ));
    }
    for (s, (rb, mv)) in real.iter().zip(model.iter()).enumerate() {
        // SAFETY: rb.ptr is a live byte array allocated this case.
        let rlen = unsafe { ba_len(rb.ptr) };
        if rlen != mv.len() {
            return Err(format!(
                "slot {s} length mismatch: real={rlen} model={}",
                mv.len()
            ));
        }
        let rbytes = unsafe { ba_bytes(rb.ptr) };
        if &rbytes != mv {
            return Err(format!(
                "slot {s} byte mismatch (B1/B5): real={rbytes:?} model={mv:?}"
            ));
        }
    }
    Ok(())
}

fn apply_ba_op(op: &ArrOp, real: &mut Vec<RealBa>, model: &mut Vec<Vec<u8>>) -> Result<(), String> {
    let n = real.len();
    match op {
        ArrOp::New { size } => {
            let ptr = runtime_new_byte_array(*size as i64);
            if (ptr as u64) < 0x1000 {
                return Err(format!(
                    "runtime_new_byte_array({size}) returned poison/null {ptr:#x}"
                ));
            }
            real.push(RealBa {
                ptr,
                backing: *size,
            });
            model.push(vec![0u8; *size]);
        }
        ArrOp::Set {
            slot,
            idx,
            len,
            val,
        } => {
            if n == 0 {
                return Ok(());
            }
            let s = slot % n;
            let size = model[s].len();
            let off = idx.resolve(size);
            // Host: silent no-op when off+len > size.
            runtime_set_byte_array(real[s].ptr, off as i64, *len as i64, *val as i64);
            if off.checked_add(*len).map(|e| e <= size).unwrap_or(false) {
                for b in &mut model[s][off..off + *len] {
                    *b = *val;
                }
            }
        }
        ArrOp::Copy {
            same,
            src_slot,
            dst_slot,
            src_off,
            dst_off,
            len,
        } => {
            if n == 0 {
                return Ok(());
            }
            let src = src_slot % n;
            let dst = if *same { src } else { dst_slot % n };
            runtime_copy_byte_array(
                real[src].ptr,
                *src_off as i64,
                real[dst].ptr,
                *dst_off as i64,
                *len as i64,
            );
            let src_ok = src_off
                .checked_add(*len)
                .map(|e| e <= model[src].len())
                .unwrap_or(false);
            let dst_ok = dst_off
                .checked_add(*len)
                .map(|e| e <= model[dst].len())
                .unwrap_or(false);
            if src_ok && dst_ok {
                if src == dst {
                    // memmove within one buffer (overlap-correct).
                    model[src].copy_within(*src_off..*src_off + *len, *dst_off);
                } else {
                    let chunk = model[src][*src_off..*src_off + *len].to_vec();
                    model[dst][*dst_off..*dst_off + *len].copy_from_slice(&chunk);
                }
            }
        }
        ArrOp::ShrinkTo { slot, new } => {
            if n == 0 {
                return Ok(());
            }
            let s = slot % n;
            let old = model[s].len();
            runtime_shrink_byte_array(real[s].ptr, *new as i64);
            // Host: only shrinks (new <= old); grow request is a no-op.
            if *new <= old {
                model[s].truncate(*new);
            }
        }
        ArrOp::ResizeTo { slot, new } => {
            if n == 0 {
                return Ok(());
            }
            let s = slot % n;
            let newptr = runtime_resize_byte_array(real[s].ptr, *new as i64);
            if (newptr as u64) < 0x1000 {
                return Err(format!(
                    "runtime_resize_byte_array(.., {new}) returned poison {newptr:#x}"
                ));
            }
            // Host freed the old buffer and returned a fresh one of size `new`.
            real[s] = RealBa {
                ptr: newptr,
                backing: *new,
            };
            model[s].resize(*new, 0); // grown bytes are zero (alloc_zeroed)
        }
        ArrOp::Compare {
            a_slot,
            b_slot,
            a_off,
            b_off,
            len,
        } => {
            if n == 0 {
                return Ok(());
            }
            let a = a_slot % n;
            let b = b_slot % n;
            let res = runtime_compare_byte_arrays(
                real[a].ptr,
                *a_off as i64,
                real[b].ptr,
                *b_off as i64,
                *len as i64,
            );
            let alen = model[a].len();
            let blen = model[b].len();
            let a_ok = a_off.checked_add(*len).map(|e| e <= alen).unwrap_or(false);
            let b_ok = b_off.checked_add(*len).map(|e| e <= blen).unwrap_or(false);
            let expected = if a_ok && b_ok {
                match model[a][*a_off..*a_off + *len].cmp(&model[b][*b_off..*b_off + *len]) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }
            } else {
                0 // host returns 0 on out-of-bounds
            };
            if res != expected {
                return Err(format!(
                    "compare result {res} != model {expected} (B1) \
                     a={a} b={b} a_off={a_off} b_off={b_off} len={len}"
                ));
            }
        }
        ArrOp::GcPoint => {
            // Allocate + free a handful of varying-size buffers via the host
            // allocator to stir the allocator's free lists (so a later resize
            // may reuse a just-freed region — UAF detector).
            for k in 0..6usize {
                let sz = (k * 37 + 13) & 0x3ff;
                let p = runtime_new_byte_array(sz as i64);
                if (p as u64) >= 0x1000 {
                    free_ba(RealBa {
                        ptr: p,
                        backing: sz,
                    });
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Boxed array op-sequence model (SmallArray# / Array#)
// ---------------------------------------------------------------------------

/// Opaque slot tokens. The host never dereferences stored slot words, so any
/// i64 is fine; a small distinct pool makes CAS-expected matching meaningful.
const TOKENS: [i64; 6] = [
    0x5000,
    0x6000,
    0x7000,
    0x4242,
    -0x1234,
    0x7fff_ffff_ffff_ff00,
];

#[derive(Clone, Debug)]
enum BoxOp {
    New {
        len: usize,
        init: i64,
    },
    /// CAS arr[idx]: if `expected_matches`, expected = current (swap happens);
    /// else expected is forced to differ (no swap). Returns old.
    Cas {
        slot: usize,
        idx: usize,
        expected_matches: bool,
        new: i64,
    },
    Clone {
        slot: usize,
        off: usize,
        len: usize,
    },
    Copy {
        same: bool,
        src_slot: usize,
        dst_slot: usize,
        src_off: usize,
        dst_off: usize,
        len: usize,
    },
    ShrinkTo {
        slot: usize,
        new: usize,
    },
    GcPoint,
}

fn token_strategy() -> impl Strategy<Value = i64> {
    prop::sample::select(TOKENS.to_vec())
}

fn small_len_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![
        4 => prop::sample::select(vec![0usize, 1, 2, 7, 8, 9, 15, 16, 17, 63, 64, 65]),
        2 => 0usize..40,
    ]
}

fn boxop_strategy() -> impl Strategy<Value = BoxOp> {
    prop_oneof![
        3 => (small_len_strategy(), token_strategy())
            .prop_map(|(len, init)| BoxOp::New { len, init }),
        5 => (0usize..8, 0usize..200, any::<bool>(), token_strategy())
            .prop_map(|(slot, idx, expected_matches, new)| BoxOp::Cas {
                slot, idx, expected_matches, new
            }),
        2 => (0usize..8, off_strategy(), small_len_strategy())
            .prop_map(|(slot, off, len)| BoxOp::Clone { slot, off, len }),
        3 => (any::<bool>(), 0usize..8, 0usize..8, off_strategy(), off_strategy(), small_len_strategy())
            .prop_map(|(same, src_slot, dst_slot, src_off, dst_off, len)| BoxOp::Copy {
                same, src_slot, dst_slot, src_off, dst_off, len
            }),
        2 => (0usize..8, small_len_strategy()).prop_map(|(slot, new)| BoxOp::ShrinkTo { slot, new }),
        1 => Just(BoxOp::GcPoint),
    ]
}

#[derive(Clone, Copy)]
struct RealBox {
    ptr: i64,
    backing: usize,
}

/// SAFETY: boxed array `[u64 len][i64 slots…]`; read length prefix.
unsafe fn box_len(ptr: i64) -> usize {
    *(ptr as *const u64) as usize
}

/// SAFETY: read the current logical slot words.
unsafe fn box_slots(ptr: i64) -> Vec<i64> {
    let n = box_len(ptr);
    std::slice::from_raw_parts((ptr as *const u8).add(8) as *const i64, n).to_vec()
}

fn free_box(b: RealBox) {
    // SAFETY: allocated by runtime_new/clone with layout (8 + 8*backing, align 8).
    unsafe {
        let layout = Layout::from_size_align(8 + 8 * b.backing, 8).unwrap();
        dealloc(b.ptr as *mut u8, layout);
    }
}

fn interp_boxed(ops: &[BoxOp]) -> Result<u64, String> {
    let _ = take_runtime_error();
    let mut real: Vec<RealBox> = Vec::new();
    let mut model: Vec<Vec<i64>> = Vec::new();

    let result = (|| -> Result<u64, String> {
        for (i, op) in ops.iter().enumerate() {
            apply_box_op(op, &mut real, &mut model).map_err(|m| format!("op#{i} {op:?}: {m}"))?;
            if let Some(e) = take_runtime_error() {
                return Err(format!(
                    "op#{i} {op:?} set runtime error {e:?} on a valid sequence (B2)"
                ));
            }
            check_box_state(&real, &model).map_err(|m| format!("after op#{i} {op:?}: {m}"))?;
        }
        let mut hash = 0xcbf29ce484222325u64;
        for a in &model {
            hash = hash.wrapping_mul(0x100000001b3) ^ (a.len() as u64);
            for &w in a {
                hash = hash.wrapping_mul(0x100000001b3) ^ (w as u64);
            }
        }
        Ok(hash)
    })();

    for &b in &real {
        free_box(b);
    }
    result
}

fn check_box_state(real: &[RealBox], model: &[Vec<i64>]) -> Result<(), String> {
    if real.len() != model.len() {
        return Err(format!(
            "slot count mismatch: real={} model={}",
            real.len(),
            model.len()
        ));
    }
    for (s, (rb, mv)) in real.iter().zip(model.iter()).enumerate() {
        // SAFETY: rb.ptr is a live boxed array.
        let rlen = unsafe { box_len(rb.ptr) };
        if rlen != mv.len() {
            return Err(format!(
                "slot {s} length mismatch: real={rlen} model={}",
                mv.len()
            ));
        }
        let rslots = unsafe { box_slots(rb.ptr) };
        if &rslots != mv {
            return Err(format!(
                "slot {s} word mismatch (B1/B5): real={rslots:?} model={mv:?}"
            ));
        }
    }
    Ok(())
}

fn apply_box_op(
    op: &BoxOp,
    real: &mut Vec<RealBox>,
    model: &mut Vec<Vec<i64>>,
) -> Result<(), String> {
    let n = real.len();
    match op {
        BoxOp::New { len, init } => {
            let ptr = runtime_new_boxed_array(*len as i64, *init);
            if (ptr as u64) < 0x1000 {
                return Err(format!(
                    "runtime_new_boxed_array({len}) returned poison {ptr:#x}"
                ));
            }
            real.push(RealBox { ptr, backing: *len });
            model.push(vec![*init; *len]);
        }
        BoxOp::Cas {
            slot,
            idx,
            expected_matches,
            new,
        } => {
            if n == 0 {
                return Ok(());
            }
            let s = slot % n;
            let size = model[s].len();
            if size == 0 {
                return Ok(()); // host: oob idx returns poison; we avoid it
            }
            let i = idx % size;
            let cur = model[s][i];
            // If we want a mismatch, pick an expected guaranteed != cur.
            let expected = if *expected_matches {
                cur
            } else {
                cur.wrapping_add(1)
            };
            let old = runtime_cas_boxed_array(real[s].ptr, i as i64, expected, *new);
            if old != cur {
                return Err(format!(
                    "CAS returned old={old} but model slot was {cur} (B1) slot={s} idx={i}"
                ));
            }
            if cur == expected {
                model[s][i] = *new;
            }
        }
        BoxOp::Clone { slot, off, len } => {
            if n == 0 {
                return Ok(());
            }
            let s = slot % n;
            let size = model[s].len();
            let in_bounds = off.checked_add(*len).map(|e| e <= size).unwrap_or(false);
            let ptr = runtime_clone_boxed_array(real[s].ptr, *off as i64, *len as i64);
            if in_bounds {
                if (ptr as u64) < 0x1000 {
                    return Err(format!(
                        "clone({off}, {len}) of size-{size} array returned poison {ptr:#x} (B2)"
                    ));
                }
                real.push(RealBox { ptr, backing: *len });
                model.push(model[s][*off..*off + *len].to_vec());
            }
            // Out-of-bounds clone returns poison with no error flag and no new
            // valid array — model adds nothing (matches host).
        }
        BoxOp::Copy {
            same,
            src_slot,
            dst_slot,
            src_off,
            dst_off,
            len,
        } => {
            if n == 0 {
                return Ok(());
            }
            let src = src_slot % n;
            let dst = if *same { src } else { dst_slot % n };
            runtime_copy_boxed_array(
                real[src].ptr,
                *src_off as i64,
                real[dst].ptr,
                *dst_off as i64,
                *len as i64,
            );
            let src_ok = src_off
                .checked_add(*len)
                .map(|e| e <= model[src].len())
                .unwrap_or(false);
            let dst_ok = dst_off
                .checked_add(*len)
                .map(|e| e <= model[dst].len())
                .unwrap_or(false);
            if src_ok && dst_ok {
                if src == dst {
                    model[src].copy_within(*src_off..*src_off + *len, *dst_off);
                } else {
                    let chunk = model[src][*src_off..*src_off + *len].to_vec();
                    model[dst][*dst_off..*dst_off + *len].copy_from_slice(&chunk);
                }
            }
        }
        BoxOp::ShrinkTo { slot, new } => {
            if n == 0 {
                return Ok(());
            }
            let s = slot % n;
            let old = model[s].len();
            runtime_shrink_boxed_array(real[s].ptr, *new as i64);
            if *new <= old {
                model[s].truncate(*new);
            }
        }
        BoxOp::GcPoint => {
            for k in 0..6usize {
                let l = (k * 5 + 3) & 0x3f;
                let p = runtime_new_boxed_array(l as i64, TOKENS[k % TOKENS.len()]);
                if (p as u64) >= 0x1000 {
                    free_box(RealBox { ptr: p, backing: l });
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Double decode / show model
// ---------------------------------------------------------------------------

/// Interesting f64 bit patterns: signed zeros, infinities, NaN payloads,
/// subnormals, the normal extremes, i64::MAX as f64, and powers of two ± 1 ulp.
fn interesting_double_bits() -> Vec<u64> {
    let mut v = vec![
        0x0000_0000_0000_0000, // +0.0
        0x8000_0000_0000_0000, // -0.0
        0x3ff0_0000_0000_0000, // 1.0
        0xbff0_0000_0000_0000, // -1.0
        0x7ff0_0000_0000_0000, // +Inf
        0xfff0_0000_0000_0000, // -Inf
        0x7ff8_0000_0000_0000, // quiet NaN
        0x7ff0_0000_0000_0001, // signalling NaN payload 1
        0xfff8_0000_0000_0abc, // negative NaN payload
        0x0000_0000_0000_0001, // smallest positive subnormal
        0x000f_ffff_ffff_ffff, // largest subnormal
        0x0010_0000_0000_0000, // smallest positive normal
        0x7fef_ffff_ffff_ffff, // DBL_MAX
        0xffef_ffff_ffff_ffff, // -DBL_MAX
        0x3fb9_9999_9999_999a, // 0.1
        0x4163_4578_0000_0000, // ~1e7 boundary region
    ];
    // i64::MAX as f64
    v.push((i64::MAX as f64).to_bits());
    v.push((i64::MIN as f64).to_bits());
    // Round scientific-notation values: single-digit mantissa, where Haskell's
    // `show` emits "1.0e10" but Rust's `{:e}` emits "1e10" (see BUG-1).
    for d in [
        1e8_f64, 1e9, 1e10, 1e20, 1e-2, 1e-5, 1e100, 2e8, 3e10, 5e9, 7e7,
    ] {
        v.push(d.to_bits());
        v.push((-d).to_bits());
    }
    // powers of two ± 1 ulp
    for p in [-1074i32, -52, -1, 0, 1, 10, 52, 53, 1023] {
        let base = 2f64.powi(p);
        let b = base.to_bits();
        v.push(b);
        v.push(b.wrapping_add(1));
        v.push(b.wrapping_sub(1));
    }
    v
}

fn double_bits_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        5 => prop::sample::select(interesting_double_bits()),
        2 => any::<u64>(),
        2 => any::<f64>().prop_map(|f| f.to_bits()),
    ]
}

/// Format a double via the host fn, reclaiming (and freeing) the leaked CString.
fn host_show_double(bits: u64) -> String {
    let p = runtime_show_double_addr(bits as i64);
    if (p as u64) < 0x1000 {
        return "<poison>".to_string();
    }
    // SAFETY: runtime_show_double_addr produced this via CString::into_raw;
    // from_raw reclaims ownership and frees on drop (round-trip is documented).
    let cs = unsafe { CString::from_raw(p as *mut c_char) };
    cs.to_string_lossy().into_owned()
}

/// Canonical show outputs we PIN (the recorded mapping for the special set).
/// Establishes the documented formatting up front; the property asserts the
/// host stays consistent with it. Returns None for values whose textual form
/// we don't pin (only determinism is then asserted).
fn canonical_show(bits: u64) -> Option<&'static str> {
    match bits {
        0x0000_0000_0000_0000 => Some("0.0"),
        0x8000_0000_0000_0000 => Some("-0.0"),
        0x3ff0_0000_0000_0000 => Some("1.0"),
        0xbff0_0000_0000_0000 => Some("-1.0"),
        0x7ff0_0000_0000_0000 => Some("Infinity"),
        0xfff0_0000_0000_0000 => Some("-Infinity"),
        0x7ff8_0000_0000_0000 => Some("NaN"),
        _ => None,
    }
}

/// Haskell's `show` for a Double in scientific notation ALWAYS writes a decimal
/// point in the mantissa ("1.0e10", "1.2345678e7"); it never emits "1e10".
/// Returns the violating output if `show(bits)` is scientific but its mantissa
/// (the part before 'e') has no '.'. Used to demonstrate BUG-1.
fn sci_mantissa_violation(bits: u64) -> Option<String> {
    let s = host_show_double(bits);
    // "Infinity"/"-Infinity"/"NaN" contain no 'e'; only true scientific output does.
    if let Some(epos) = s.find(['e', 'E']) {
        let mantissa = &s[..epos];
        if !mantissa.contains('.') {
            return Some(s);
        }
    }
    None
}

fn check_double(bits: u64) -> Result<(), String> {
    let _ = take_runtime_error();
    let d = f64::from_bits(bits);

    // --- show: determinism (B4 self-consistency) + pinned canonical mapping ---
    let s1 = host_show_double(bits);
    let s2 = host_show_double(bits);
    if s1 != s2 {
        return Err(format!(
            "show({bits:#018x}) nondeterministic: {s1:?} vs {s2:?} (B4)"
        ));
    }
    if let Some(exp) = canonical_show(bits) {
        if s1 != exp {
            return Err(format!(
                "show({bits:#018x}) = {s1:?} but pinned canonical is {exp:?} (B1)"
            ));
        }
    }
    // NaN/Inf must never render as a finite-looking decimal.
    if d.is_nan() && s1 != "NaN" {
        return Err(format!("NaN rendered as {s1:?} (B1)"));
    }
    if d.is_infinite() && !(s1 == "Infinity" || s1 == "-Infinity") {
        return Err(format!("Inf rendered as {s1:?} (B1)"));
    }

    // --- decode: determinism ---
    let m1 = runtime_decode_double_mantissa(bits as i64);
    let e1 = runtime_decode_double_exponent(bits as i64);
    let m2 = runtime_decode_double_mantissa(bits as i64);
    let e2 = runtime_decode_double_exponent(bits as i64);
    if (m1, e1) != (m2, e2) {
        return Err(format!(
            "decode({bits:#018x}) nondeterministic: ({m1},{e1}) vs ({m2},{e2}) (B4)"
        ));
    }

    // --- decode: documented sentinels for the non-finite / zero cases ---
    if d == 0.0 || d.is_nan() {
        if (m1, e1) != (0, 0) {
            return Err(format!(
                "decode of {d:?} should be (0,0), got ({m1},{e1}) (B1)"
            ));
        }
        return Ok(());
    }
    if d.is_infinite() {
        let want = if d > 0.0 { 1 } else { -1 };
        if m1 != want || e1 != 0 {
            return Err(format!(
                "decode of {d:?} should be ({want},0), got ({m1},{e1}) (B1)"
            ));
        }
        return Ok(());
    }

    // --- decode: structural invariants (exact, all finite nonzero) ---
    // Mantissa is normalised to have no trailing zeros → odd (nonzero), and
    // bounded by 2^53 (52-bit fraction + implicit leading bit).
    if m1 == 0 {
        return Err(format!("decode of nonzero {d:?} gave zero mantissa (B1)"));
    }
    if m1 % 2 == 0 {
        return Err(format!(
            "decode mantissa {m1} of {d:?} has trailing zero — not normalised (B1)"
        ));
    }
    if m1.unsigned_abs() > (1u64 << 53) {
        return Err(format!("decode mantissa {m1} of {d:?} exceeds 2^53 (B1)"));
    }

    // --- decode/encode identity in the magnitude band where the float
    // reconstruction is provably exact (exponent stays well inside the normal
    // range, so 2^e1 is exact and mantissa·2^e1 does not under/overflow). ---
    let abs = d.abs();
    if (1e-200..=1e200).contains(&abs) {
        let recon = (m1 as f64) * 2f64.powi(e1 as i32);
        if recon != d {
            return Err(format!(
                "decode/encode mismatch for {d:?} (bits {bits:#018x}): \
                 mantissa={m1} exp={e1} recon={recon:?} (B1)"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    /// ByteArray op-sequence model equivalence + fatal-signal hunt.
    #[test]
    #[serial]
    fn bytearray_model(ops in prop::collection::vec(arrop_strategy(), 1..40)) {
        let ops2 = ops.clone();
        let outcome = run_forked(move || interp_bytearray(&ops2).map(|_| ()));
        assert_outcome!(outcome, ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    /// Boxed-array (incl. CAS) op-sequence model equivalence + signal hunt.
    #[test]
    #[serial]
    fn boxed_array_model(ops in prop::collection::vec(boxop_strategy(), 1..40)) {
        let ops2 = ops.clone();
        let outcome = run_forked(move || interp_boxed(&ops2).map(|_| ()));
        assert_outcome!(outcome, ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    /// GC/allocator-churn substitution for the nursery A/B oracle (B4):
    /// run the SAME ByteArray sequence twice (with GcPoint churn baked into the
    /// generator) and require identical final state. Divergence ⇒ a malloc'd
    /// buffer leaked allocator nondeterminism (uninitialised read / UAF).
    #[test]
    #[serial]
    fn bytearray_gc_run_twice(ops in prop::collection::vec(arrop_strategy(), 1..40)) {
        let ops2 = ops.clone();
        let outcome = run_forked(move || {
            let h1 = interp_bytearray(&ops2)?;
            let h2 = interp_bytearray(&ops2)?;
            if h1 != h2 {
                return Err(format!("run-twice final-state divergence {h1:#x} != {h2:#x} (B4)"));
            }
            Ok(())
        });
        assert_outcome!(outcome, ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

    /// Double decode/show: determinism, pinned canonical formatting, decode
    /// sentinels, structural invariants, and decode/encode identity.
    #[test]
    #[serial]
    fn double_decode_show(bits in double_bits_strategy()) {
        let outcome = run_forked(move || check_double(bits));
        assert_outcome!(outcome, format!("bits={bits:#018x} d={:?}", f64::from_bits(bits)));
    }
}

// ---------------------------------------------------------------------------
// BUG-1: haskell_show_double drops the mantissa decimal point in scientific
// notation. host_fns.rs `haskell_show_double` (~1893) formats |x| >= 1e7 (and
// |x| < 0.1) via Rust `format!("{:e}", d)`, which renders e.g. 1e10 as "1e10".
// Haskell's `show (1e10 :: Double)` is "1.0e10" — the mantissa always carries a
// decimal point. The function's doc claims it matches Haskell's `show`.
//
// Class: B1 (model/contract mismatch). Host fn: runtime_show_double_addr /
// haskell_show_double. Observed: "1e10". Expected: "1.0e10".
//
// This property ASSERTS the documented Haskell invariant and therefore FAILS;
// it is #[ignore]d so the suite stays green, but its shrunk counterexample is
// persisted in tests/proptest-regressions/proptest_host_arrays.txt (committed).
// Remove the #[ignore] after the bug is fixed.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

    // BUG-1 FIXED 2026-06-10: haskell_show_double now inserts ".0" before the
    // exponent when {:e} omits the mantissa decimal point. Active regression
    // property (500 cases, fork-contained).
    #[test]
    #[serial]
    fn bug1_show_double_scientific_decimal(bits in double_bits_strategy()) {
        let outcome = run_forked(move || match sci_mantissa_violation(bits) {
            Some(s) => Err(format!(
                "show({bits:#018x}) = {s:?} — scientific mantissa has no '.' \
                 (Haskell show would write e.g. \"1.0e10\")"
            )),
            None => Ok(()),
        });
        assert_outcome!(outcome, format!("bits={bits:#018x} d={:?}", f64::from_bits(bits)));
    }
}

/// Deterministic minimal repro for BUG-1 (no proptest). `bits = 1` (the
/// smallest positive subnormal, 5e-324) is the shrunk proptest witness; 1e10 is
/// the human-readable witness. Host renders "5e-324" / "1e10"; Haskell `show`
/// renders "5.0e-324" / "1.0e10" — the mantissa always carries a decimal point.
// BUG-1 FIXED 2026-06-10 — active regression test.
#[test]
#[serial]
fn bug1_repro_minimal() {
    // Minimal shrunk witness from the committed regression seed.
    let got_min = host_show_double(1u64);
    assert_eq!(
        got_min, "5.0e-324",
        "BUG-1: runtime_show_double_addr produced {got_min:?} for the smallest \
         subnormal; Haskell `show` produces \"5.0e-324\""
    );
    // Human-readable witness.
    let got_1e10 = host_show_double(1e10_f64.to_bits());
    assert_eq!(
        got_1e10, "1.0e10",
        "BUG-1: runtime_show_double_addr produced {got_1e10:?} for 1e10; \
         Haskell `show` produces \"1.0e10\" (scientific mantissa needs a decimal point)"
    );
}

// ---------------------------------------------------------------------------
// Fencepost / overlap coverage (counter-asserted, deterministic).
//
// Samples the strategies directly (no host calls, no fork) and tallies. This
// proves the generators actually exercise the alignment/word/page fenceposts
// and that a meaningful fraction of copies overlap (memmove territory).
// ---------------------------------------------------------------------------

#[test]
fn fencepost_and_overlap_coverage() {
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let mut size_hits = std::collections::HashMap::<usize, u64>::new();
    let mut copy_total = 0u64;
    let mut copy_overlap = 0u64;

    // Sample many op sequences and tally New sizes + Copy overlap.
    let seq = prop::collection::vec(arrop_strategy(), 1..40);
    for _ in 0..1500 {
        let tree = seq.new_tree(&mut runner).unwrap();
        for op in tree.current() {
            match op {
                ArrOp::New { size } => {
                    *size_hits.entry(size).or_insert(0) += 1;
                }
                ArrOp::Copy {
                    same,
                    src_off,
                    dst_off,
                    len,
                    ..
                } => {
                    copy_total += 1;
                    // Generation-level overlap proxy: same array + intersecting
                    // [off, off+len) ranges. (Runtime bounds may turn some into
                    // no-ops, but this proves the generator MAKES overlaps.)
                    if same && ranges_overlap(src_off, dst_off, len) {
                        copy_overlap += 1;
                    }
                }
                _ => {}
            }
        }
    }

    // Each fencepost size must be well-represented.
    for &fp in FENCEPOSTS.iter() {
        let hits = size_hits.get(&fp).copied().unwrap_or(0);
        assert!(
            hits >= 20,
            "fencepost size {fp} hit only {hits} times (need >= 20). all: {size_hits:?}"
        );
    }

    // Overlapping copies must be a non-trivial fraction of all copies.
    assert!(copy_total > 0, "no Copy ops were generated");
    let frac = copy_overlap as f64 / copy_total as f64;
    assert!(
        frac >= 0.10,
        "overlapping copies were only {:.1}% of {} copies (need >= 10%)",
        frac * 100.0,
        copy_total
    );
    eprintln!(
        "coverage: {} copies, {} overlapping ({:.1}%); fencepost hits: {:?}",
        copy_total,
        copy_overlap,
        frac * 100.0,
        size_hits
    );
}

// ---------------------------------------------------------------------------
// Smoke tests (fast, non-prop) — basic correctness + a regression anchor.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn smoke_bytearray_basic() {
    let ops = vec![
        ArrOp::New { size: 64 },
        ArrOp::Set {
            slot: 0,
            idx: IdxKind::First,
            len: 8,
            val: 0xAB,
        },
        ArrOp::New { size: 8 },
        ArrOp::Copy {
            same: false,
            src_slot: 0,
            dst_slot: 1,
            src_off: 0,
            dst_off: 0,
            len: 8,
        },
        ArrOp::Compare {
            a_slot: 0,
            b_slot: 1,
            a_off: 0,
            b_off: 0,
            len: 8,
        },
        ArrOp::ResizeTo { slot: 0, new: 4096 },
        ArrOp::ShrinkTo { slot: 0, new: 7 },
    ];
    interp_bytearray(&ops).expect("basic bytearray sequence");
}

#[test]
#[serial]
fn smoke_bytearray_overlap_copy() {
    // Overlapping intra-array memmove: shift bytes forward by 1.
    let ops = vec![
        ArrOp::New { size: 16 },
        ArrOp::Set {
            slot: 0,
            idx: IdxKind::At(0),
            len: 8,
            val: 0x11,
        },
        ArrOp::Copy {
            same: true,
            src_slot: 0,
            dst_slot: 0,
            src_off: 0,
            dst_off: 1,
            len: 8,
        },
    ];
    interp_bytearray(&ops).expect("overlapping copy sequence");
}

#[test]
#[serial]
fn smoke_boxed_cas() {
    let ops = vec![
        BoxOp::New {
            len: 4,
            init: TOKENS[0],
        },
        BoxOp::Cas {
            slot: 0,
            idx: 1,
            expected_matches: true,
            new: TOKENS[1],
        },
        BoxOp::Cas {
            slot: 0,
            idx: 1,
            expected_matches: false,
            new: TOKENS[2],
        },
        BoxOp::Clone {
            slot: 0,
            off: 1,
            len: 2,
        },
        BoxOp::ShrinkTo { slot: 0, new: 2 },
    ];
    interp_boxed(&ops).expect("basic boxed/CAS sequence");
}

#[test]
#[serial]
fn smoke_doubles() {
    for &bits in interesting_double_bits().iter() {
        check_double(bits).unwrap_or_else(|e| panic!("double {bits:#018x}: {e}"));
    }
}
