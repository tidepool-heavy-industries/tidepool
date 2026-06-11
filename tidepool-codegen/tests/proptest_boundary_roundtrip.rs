//! Workstream W3: boundary-roundtrip property tests.
//!
//! Hunts bugs in `value_to_heap` / `heap_to_value_forcing` (tidepool-codegen
//! `heap_bridge`) and `FromCore`/`ToCore` (tidepool-bridge `impls`).
//!
//! ALL `Value` construction, comparison, canonicalization, and teardown here is
//! ITERATIVE (worklist / fold over `Vec`). Recursive `Value` walks would
//! themselves overflow the host thread on the deep-chain arm — a documented bug
//! class in this repo: deep `Value` spines kill host threads via recursive
//! `Drop`. See [`flatten_drop`] / [`values_eq_iter`] / [`canonicalize`].
//!
//! Cap-straddling cases (field counts around `MAX_FIELDS = 1024`, depths around
//! `MAX_DEPTH = 10_000`) run in a forked child via `libc::fork` so a fatal
//! signal (SIGSEGV/SIGILL/SIGBUS/SIGABRT) in the child is observed as a finding
//! (B3) by the parent rather than killing the test runner.
//!
//! Every `#[test]` in this file is `#[serial]`. The fork containment harness
//! allocates in the child immediately after `fork()`; if another test thread
//! held the allocator lock at fork time, the child's first `malloc` would
//! deadlock against a lock that is never released in the child. Serializing the
//! whole binary guarantees no other test thread is mid-allocation when we fork.

use proptest::prelude::*;
use serial_test::serial;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tidepool_bridge::{FromCore, ToCore};
use tidepool_codegen::context::VMContext;
use tidepool_codegen::heap_bridge::{
    bump_alloc_from_vmctx, heap_to_value_forcing, value_to_heap, BridgeError,
};
use tidepool_codegen::nursery::Nursery;
use tidepool_eval::value::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable, Literal, SrcBang};

// ---------------------------------------------------------------------------
// VMContext setup (mirrors heap_bridge.rs / heap_bridge_tests.rs).
// ---------------------------------------------------------------------------

extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

/// Build a nursery and a `VMContext` pointing into it. The returned `Nursery`
/// must outlive the `VMContext`: the context holds raw pointers into the
/// nursery's heap buffer. Moving the `Nursery` value moves only its `Vec`
/// control block, not the buffer, so the raw pointers stay valid — the same
/// pattern the in-crate tests rely on.
fn setup_vmctx(size: usize) -> (Nursery, VMContext) {
    let mut nursery = Nursery::new(size);
    let vmctx = nursery.make_vmctx(mock_gc_trigger);
    (nursery, vmctx)
}

// ---------------------------------------------------------------------------
// Iterative `Value` canonicalizer — a deliverable in its own right.
//
// Maps EQUIVALENT representations to a single canonical form so roundtrip
// identity can be checked structurally, and NOTHING ELSE. Genuine content
// differences are preserved.
//
// Canonical rules (semantics-preserving only):
//   * `Value::ByteArray(bytes)`  ->  `Lit(LitString(bytes))`
//       (the read side decodes `LIT_TAG_BYTEARRAY` back to `ByteArray` and
//        `LIT_TAG_STRING` back to `LitString`; both carry the same wire bytes,
//        so they are the same value for boundary purposes.)
//   * NaN float / double bit-patterns  ->  one canonical NaN
//       (round-trip is bit-exact, but distinct NaN encodings are still the same
//        value; only collapsed for equality, never altered on the wire.)
//   * everything else: structural identity.
//
// We deliberately do NOT collapse the three STRING reprs — `Text(ba,off,len)`,
// `[Char]` cons-list, `LitString` — to one form here. Those are equivalent at
// the *decoded Rust `String`* level, not at the `Value` level, and are checked
// by the dedicated triple-equality property, which compares decoded `String`s.
// Collapsing them here would canonicalize away genuine `Value`-level structure.
// ---------------------------------------------------------------------------

fn canon_literal(l: &Literal) -> Literal {
    match l {
        Literal::LitFloat(bits) => {
            if f32::from_bits(*bits as u32).is_nan() {
                Literal::LitFloat(f32::NAN.to_bits() as u64)
            } else {
                Literal::LitFloat(*bits)
            }
        }
        Literal::LitDouble(bits) => {
            if f64::from_bits(*bits).is_nan() {
                Literal::LitDouble(f64::NAN.to_bits())
            } else {
                Literal::LitDouble(*bits)
            }
        }
        other => other.clone(),
    }
}

/// Iterative post-order canonicalizer. Rebuilds the tree bottom-up using an
/// explicit work stack — never recurses into Rust call frames.
fn canonicalize(v: &Value) -> Value {
    enum Frame<'a> {
        Enter(&'a Value),
        /// Reassemble a `Con` of this tag by popping `nfields` rebuilt values
        /// off the output stack.
        ExitCon(DataConId, usize),
    }
    let mut work: Vec<Frame> = vec![Frame::Enter(v)];
    let mut out: Vec<Value> = Vec::new();

    while let Some(frame) = work.pop() {
        match frame {
            Frame::Enter(node) => match node {
                Value::Lit(l) => out.push(Value::Lit(canon_literal(l))),
                Value::ByteArray(bs) => {
                    let bytes = bs.lock().map(|g| g.clone()).unwrap_or_default();
                    out.push(Value::Lit(Literal::LitString(bytes)));
                }
                Value::Con(id, fields) => {
                    work.push(Frame::ExitCon(*id, fields.len()));
                    for f in fields {
                        work.push(Frame::Enter(f));
                    }
                }
                // Non-bridgeable shapes (closures, thunks, ...) never occur in
                // these tests; pass them through structurally if they ever do.
                other => out.push(other.clone()),
            },
            Frame::ExitCon(id, n) => {
                let mut fields = Vec::with_capacity(n);
                for _ in 0..n {
                    fields.push(out.pop().expect("canon: output underflow"));
                }
                fields.reverse();
                out.push(Value::Con(id, fields));
            }
        }
    }
    out.pop().expect("canon: empty output")
}

// ---------------------------------------------------------------------------
// Iterative structural equality (worklist). NEVER recurse.
// ---------------------------------------------------------------------------

fn lits_eq(a: &Literal, b: &Literal) -> bool {
    use Literal::*;
    match (a, b) {
        (LitInt(x), LitInt(y)) => x == y,
        (LitWord(x), LitWord(y)) => x == y,
        (LitChar(x), LitChar(y)) => x == y,
        (LitString(x), LitString(y)) => x == y,
        (LitFloat(x), LitFloat(y)) => {
            let (fx, fy) = (f32::from_bits(*x as u32), f32::from_bits(*y as u32));
            (fx.is_nan() && fy.is_nan()) || x == y
        }
        (LitDouble(x), LitDouble(y)) => {
            let (fx, fy) = (f64::from_bits(*x), f64::from_bits(*y));
            (fx.is_nan() && fy.is_nan()) || x == y
        }
        _ => false,
    }
}

fn values_eq_iter(a: &Value, b: &Value) -> bool {
    let mut stack: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((x, y)) = stack.pop() {
        match (x, y) {
            (Value::Lit(la), Value::Lit(lb)) => {
                if !lits_eq(la, lb) {
                    return false;
                }
            }
            (Value::Con(ta, fa), Value::Con(tb, fb)) => {
                if ta != tb || fa.len() != fb.len() {
                    return false;
                }
                for (cx, cy) in fa.iter().zip(fb.iter()) {
                    stack.push((cx, cy));
                }
            }
            _ => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Iterative deep-`Drop` helper. After building giant chains, `Value`'s own
// `Drop` is recursive; flatten before drop to avoid a host stack overflow on
// cleanup.
// ---------------------------------------------------------------------------

fn flatten_drop(v: Value) {
    let mut stack: Vec<Value> = vec![v];
    while let Some(mut node) = stack.pop() {
        // Drain children via `&mut` (cannot move out of a `Drop` type): the
        // node's field vec is left empty, so when `node` drops at the end of
        // this iteration its `Drop` is shallow — no recursion.
        if let Value::Con(_, fields) = &mut node {
            for f in fields.drain(..) {
                stack.push(f);
            }
        }
        // Lit / ByteArray drop trivially.
    }
}

// ---------------------------------------------------------------------------
// Standard DataConTable.
// ---------------------------------------------------------------------------

const CON_NIL: u64 = 5;
const CON_CONS: u64 = 6;
const CON_TEXT: u64 = 15;
const CON_CHAR: u64 = 13;

fn std_table() -> DataConTable {
    let mut t = DataConTable::new();
    let mut add = |id: u64, name: &str, arity: u32| {
        t.insert(DataCon {
            id: DataConId(id),
            name: name.into(),
            tag: 1,
            rep_arity: arity,
            field_bangs: vec![SrcBang::NoSrcBang; arity as usize],
            qualified_name: None,
        });
    };
    add(0, "Nothing", 0);
    add(1, "Just", 1);
    add(2, "False", 0);
    add(3, "True", 0);
    add(4, "(,)", 2);
    add(5, "[]", 0);
    add(6, ":", 2);
    add(7, "(,,)", 3);
    add(8, "Right", 1);
    add(9, "Left", 1);
    add(10, "I#", 1);
    add(11, "W#", 1);
    add(12, "D#", 1);
    add(13, "C#", 1);
    add(14, "()", 0);
    add(15, "Text", 3);
    add(16, "ByteArray", 1);
    t
}

// ---------------------------------------------------------------------------
// Iterative value generators.
// ---------------------------------------------------------------------------

fn arb_literal() -> impl Strategy<Value = Literal> {
    prop_oneof![
        any::<i64>().prop_map(Literal::LitInt),
        any::<u64>().prop_map(Literal::LitWord),
        any::<char>().prop_map(Literal::LitChar),
        any::<u64>().prop_map(Literal::LitDouble),
        // LitFloat carries an f32 bit-pattern in the low 32 bits.
        any::<u32>().prop_map(|b| Literal::LitFloat(b as u64)),
        // Bounded byte content: `runtime_new_byte_array` LEAKS (no free), so
        // keep payloads small across the case count.
        proptest::collection::vec(any::<u8>(), 0..32).prop_map(Literal::LitString),
    ]
}

/// `i64` / `u64` / `f64` extremes that exercise sign-bit, boxing, and the
/// IEEE-754 special encodings.
fn arb_extreme_literal() -> impl Strategy<Value = Literal> {
    prop_oneof![
        Just(Literal::LitInt(i64::MIN)),
        Just(Literal::LitInt(i64::MAX)),
        Just(Literal::LitInt(i64::MIN + 1)),
        Just(Literal::LitInt(-1)),
        Just(Literal::LitInt(0)),
        Just(Literal::LitInt(1)),
        Just(Literal::LitInt(1i64 << 62)),
        Just(Literal::LitWord(u64::MAX)),
        Just(Literal::LitWord(0)),
        Just(Literal::LitWord(1u64 << 63)),
        Just(Literal::LitDouble(f64::NAN.to_bits())),
        Just(Literal::LitDouble(f64::INFINITY.to_bits())),
        Just(Literal::LitDouble(f64::NEG_INFINITY.to_bits())),
        Just(Literal::LitDouble(0.0f64.to_bits())),
        Just(Literal::LitDouble((-0.0f64).to_bits())),
        Just(Literal::LitDouble(f64::MIN_POSITIVE.to_bits())),
        Just(Literal::LitDouble(1u64)), // smallest subnormal
        Just(Literal::LitFloat(f32::NAN.to_bits() as u64)),
        Just(Literal::LitFloat((-0.0f32).to_bits() as u64)),
        Just(Literal::LitFloat(f32::INFINITY.to_bits() as u64)),
    ]
}

/// A typical small `Value` tree (size 1..50, mixed Con/Lit/ByteArray), built by
/// `prop_recursive` (which is itself iterative internally). Depth is capped so
/// the decoded `Value`'s ordinary recursive `Drop` is safe in-process.
fn arb_typical_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        arb_literal().prop_map(Value::Lit),
        arb_extreme_literal().prop_map(Value::Lit),
        proptest::collection::vec(any::<u8>(), 0..16)
            .prop_map(|b| Value::ByteArray(Arc::new(Mutex::new(b)))),
    ];
    leaf.prop_recursive(5, 50, 6, |inner| {
        (0u64..20, proptest::collection::vec(inner, 0..6))
            .prop_map(|(id, fs)| Value::Con(DataConId(id), fs))
    })
}

/// Build a Haskell-style list `x0 : x1 : ... : []` iteratively (foldr).
fn make_list(elems: Vec<Value>) -> Value {
    let mut acc = Value::Con(DataConId(CON_NIL), vec![]);
    for e in elems.into_iter().rev() {
        acc = Value::Con(DataConId(CON_CONS), vec![e, acc]);
    }
    acc
}

fn make_tuple2(a: Value, b: Value) -> Value {
    Value::Con(DataConId(4), vec![a, b])
}

/// Mixed nesting: a tuple of (list-of-strings-in-various-reprs, a string).
/// Exercises Con/Lit/ByteArray interleaved at a few levels, all bounded.
fn arb_mixed_value() -> impl Strategy<Value = Value> {
    (arb_string_content(), arb_string_content(), arb_pad()).prop_map(|(s1, s2, pad)| {
        let strs = vec![
            litstring_repr(&s1),
            text_repr_with_offset(s2.as_bytes(), &pad),
            charlist_repr(&s1),
        ];
        make_tuple2(make_list(strs), litstring_repr(&s2))
    })
}

// ---------------------------------------------------------------------------
// String repr builders (iterative). Same content -> several reprs.
// ---------------------------------------------------------------------------

/// `Text(ByteArray, off, len)` with offset = `pad.len()`: prepend `pad` junk
/// bytes, then point `off`/`len` at the real content. `pad` is ASCII so it
/// never splits the UTF-8 boundary of `content`.
fn text_repr_with_offset(content: &[u8], pad: &[u8]) -> Value {
    let mut backing = Vec::with_capacity(pad.len() + content.len());
    backing.extend_from_slice(pad);
    backing.extend_from_slice(content);
    Value::Con(
        DataConId(CON_TEXT),
        vec![
            Value::ByteArray(Arc::new(Mutex::new(backing))),
            Value::Lit(Literal::LitInt(pad.len() as i64)),
            Value::Lit(Literal::LitInt(content.len() as i64)),
        ],
    )
}

/// `[Char]` cons-list of bare `LitChar`. Built iteratively.
fn charlist_repr(s: &str) -> Value {
    let elems: Vec<Value> = s.chars().map(|c| Value::Lit(Literal::LitChar(c))).collect();
    make_list(elems)
}

/// `[Char]` cons-list where each char is boxed in `C#`. Built iteratively.
fn charlist_boxed_repr(s: &str) -> Value {
    let elems: Vec<Value> = s
        .chars()
        .map(|c| Value::Con(DataConId(CON_CHAR), vec![Value::Lit(Literal::LitChar(c))]))
        .collect();
    make_list(elems)
}

fn litstring_repr(s: &str) -> Value {
    Value::Lit(Literal::LitString(s.as_bytes().to_vec()))
}

fn arb_string_content() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("a".to_string()),
        Just("λ".to_string()),
        Just("日本語".to_string()),
        Just("héllo wörld".to_string()),
        Just("\u{1F600}emoji".to_string()),
        "[a-zA-Z0-9 λ日💀]{0,24}",
    ]
}

/// Padding for the `Text` repr. Heavily weighted toward NONZERO offsets (the
/// spec's emphasis: slice semantics with a non-zero start), with the empty pad
/// retained so the zero-offset case is still covered.
fn arb_pad() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        4 => proptest::collection::vec(0x20u8..=0x7e, 1..8), // nonzero offset
        1 => Just(Vec::new()),                                // zero offset
    ]
}

// ---------------------------------------------------------------------------
// Roundtrip primitive and nursery sizing.
// ---------------------------------------------------------------------------

/// `value_to_heap` then `heap_to_value_forcing` through a fresh nursery.
///
/// # Safety
/// The nursery is created and kept alive inside this function; the returned
/// `Value` owns no heap pointers, so it is valid after the nursery drops.
unsafe fn heap_roundtrip(v: &Value, nursery_bytes: usize) -> Result<Value, BridgeError> {
    let (_nursery, mut vmctx) = setup_vmctx(nursery_bytes);
    let ptr = value_to_heap(v, &mut vmctx)?;
    heap_to_value_forcing(ptr, &mut vmctx as *mut VMContext)
}

/// Comfortable nursery size for a value via an ITERATIVE node count.
fn estimate_nursery(v: &Value) -> usize {
    let mut count = 0usize;
    let mut stack = vec![v];
    while let Some(node) = stack.pop() {
        count += 1;
        if let Value::Con(_, fields) = node {
            for f in fields {
                stack.push(f);
            }
        }
    }
    // Con: 24 + 8*fields; Lit: 24. Pad generously, plus a fixed floor.
    count.saturating_mul(256).max(4096) + 64 * 1024
}

// ---------------------------------------------------------------------------
// Reach counters — cap-straddling coverage.
// ---------------------------------------------------------------------------

static FIELDS_1023: AtomicUsize = AtomicUsize::new(0);
static FIELDS_1024: AtomicUsize = AtomicUsize::new(0);
static FIELDS_1025: AtomicUsize = AtomicUsize::new(0);
static FIELDS_TOTAL: AtomicUsize = AtomicUsize::new(0);
static DEPTH_9999: AtomicUsize = AtomicUsize::new(0);
static DEPTH_10000: AtomicUsize = AtomicUsize::new(0);
static DEPTH_10001: AtomicUsize = AtomicUsize::new(0);
static DEPTH_TOTAL: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Fork containment for B3 (fatal-signal) hunting.
//
// The child runs `body` under `catch_unwind`, writes one status byte (0 =
// ok/clean-Err, 1 = Rust panic) to a pipe, then `_exit(0)`. The parent
// `waitpid`s and inspects `WIFSIGNALED`: a fatal signal in the child is B3.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[derive(Debug, PartialEq)]
enum ForkOutcome {
    Ok,
    ChildPanicked,
    Signaled(i32),
}

#[cfg(unix)]
fn run_forked<F: FnOnce()>(body: F) -> ForkOutcome {
    use std::os::unix::io::RawFd;
    let mut fds: [RawFd; 2] = [0; 2];
    // SAFETY: standard pipe(2) into a valid 2-element array.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed");
    let (read_fd, write_fd) = (fds[0], fds[1]);

    // SAFETY: fork(2); both branches are handled below.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork() failed");

    if pid == 0 {
        // CHILD. SAFETY: close the read end we do not use.
        unsafe { libc::close(read_fd) };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        let byte: [u8; 1] = if result.is_ok() { [0] } else { [1] };
        // SAFETY: write one status byte, close, and _exit without running
        // atexit handlers (which would touch parent-shared state).
        unsafe {
            libc::write(write_fd, byte.as_ptr() as *const libc::c_void, 1);
            libc::close(write_fd);
            libc::_exit(0);
        }
    }

    // PARENT. SAFETY: close the write end we do not use.
    unsafe { libc::close(write_fd) };
    let mut byte: [u8; 1] = [255];
    // SAFETY: read up to one status byte from the child.
    let n = unsafe { libc::read(read_fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
    // SAFETY: close the read end.
    unsafe { libc::close(read_fd) };

    let mut status: libc::c_int = 0;
    // SAFETY: waitpid on the known child pid.
    unsafe { libc::waitpid(pid, &mut status, 0) };

    // WIFSIGNALED / WTERMSIG via manual bit inspection (portable on Linux).
    let signaled = (status & 0x7f) != 0 && (status & 0x7f) != 0x7f;
    if signaled {
        return ForkOutcome::Signaled(status & 0x7f);
    }
    if n == 1 && byte[0] == 1 {
        return ForkOutcome::ChildPanicked;
    }
    ForkOutcome::Ok
}

/// A deep cons chain (2-field `:` cells) of `depth` levels, terminal
/// `Lit(LitInt(0))`. Built by ITERATIVE fold — never recursively.
fn build_deep_chain(depth: usize) -> Value {
    let mut acc = Value::Lit(Literal::LitInt(0));
    for i in 0..depth {
        acc = Value::Con(
            DataConId(CON_CONS),
            vec![Value::Lit(Literal::LitInt(i as i64)), acc],
        );
    }
    acc
}

// ===========================================================================
// PROPERTY 1: Roundtrip identity (x500).
//
// `heap_to_value_forcing(value_to_heap(v))` must equal `v` under the string-
// repr canonicalizer, via worklist equality. Equivalent reprs disagreeing
// post-canonicalization would be B5.
//
// The generator follows the spec's weighting for the arms that are safe to
// roundtrip and structurally compare IN-PROCESS (bounded depth): typical (3),
// extremes (2), the three string reprs (3), and mixed nesting (1). The wide-Con
// and deep-chain arms are cap-straddling crash hunts and run forked in
// Property 5, not here.
// ===========================================================================

fn arb_roundtrip_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        3 => arb_typical_value(),
        2 => arb_extreme_literal().prop_map(Value::Lit),
        3 => (arb_string_content(), arb_pad(), 0u8..3u8).prop_map(|(s, pad, which)| {
            match which {
                0 => text_repr_with_offset(s.as_bytes(), &pad),
                1 => charlist_repr(&s),
                _ => litstring_repr(&s),
            }
        }),
        1 => arb_mixed_value(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    #[serial]
    fn prop_roundtrip_identity(v in arb_roundtrip_value()) {
        let nursery = estimate_nursery(&v);
        let canon_in = canonicalize(&v);
        // SAFETY: nursery is owned inside heap_roundtrip and the result owns no
        // heap pointers.
        let out = unsafe { heap_roundtrip(&v, nursery) };
        match out {
            Ok(decoded) => {
                let canon_out = canonicalize(&decoded);
                let eq = values_eq_iter(&canon_in, &canon_out);
                let report = if eq {
                    String::new()
                } else {
                    format!("\n in:  {:?}\n out: {:?}", canon_in, canon_out)
                };
                flatten_drop(decoded);
                flatten_drop(canon_out);
                flatten_drop(canon_in);
                prop_assert!(eq, "B5 roundtrip mismatch{}", report);
            }
            Err(e) => {
                flatten_drop(canon_in);
                // These arms are all within documented limits; an error here is
                // itself a finding.
                prop_assert!(false, "unexpected bridge error on roundtrippable value: {:?}", e);
            }
        }
    }
}

// ===========================================================================
// PROPERTY 2: GC-pressure roundtrip (x200).
//
// Small (4 KiB) nursery; junk allocations between `value_to_heap` and readback.
// The decode must either reproduce the value or refuse cleanly with
// `NurseryExhausted` — never corrupt or crash.
//
// NOTE: `mock_gc_trigger` is a no-op, so no real Cheney copy occurs; this
// exercises bump-allocation pressure and readback stability, not live-object
// relocation. Relocation rooting is covered by the in-crate GC tests that own a
// real collector. This limitation is recorded in the findings ledger.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    #[serial]
    fn prop_roundtrip_gc_pressure(v in arb_typical_value()) {
        let canon_in = canonicalize(&v);
        let (_nursery, mut vmctx) = setup_vmctx(4 * 1024);
        // SAFETY: vmctx is valid for the lifetime of _nursery.
        let res = unsafe { value_to_heap(&v, &mut vmctx) };
        match res {
            Ok(ptr) => {
                // Consume remaining nursery to apply allocation pressure.
                // SAFETY: bump-alloc within the same live nursery.
                unsafe {
                    for _ in 0..32 {
                        let _ = bump_alloc_from_vmctx(&mut vmctx, 64);
                    }
                }
                // SAFETY: ptr came from value_to_heap into the still-live nursery.
                let decoded = unsafe { heap_to_value_forcing(ptr, &mut vmctx as *mut VMContext) };
                match decoded {
                    Ok(d) => {
                        let canon_out = canonicalize(&d);
                        let eq = values_eq_iter(&canon_in, &canon_out);
                        let report = if eq { String::new() } else { format!("{:?}", canon_out) };
                        flatten_drop(d);
                        flatten_drop(canon_out);
                        flatten_drop(canon_in);
                        prop_assert!(eq, "B4 gc-pressure roundtrip mismatch: {}", report);
                    }
                    Err(e) => {
                        flatten_drop(canon_in);
                        prop_assert!(false, "gc-pressure readback error: {:?}", e);
                    }
                }
            }
            // Clean refusal under pressure is acceptable.
            Err(BridgeError::NurseryExhausted) => flatten_drop(canon_in),
            Err(e) => {
                flatten_drop(canon_in);
                prop_assert!(false, "unexpected encode error under pressure: {:?}", e);
            }
        }
    }
}

// ===========================================================================
// PROPERTY 3: String triple-equality (x300).
//
// All reprs of the same content decode to the same Rust `String` via
// `FromCore`. Includes multi-byte UTF-8 sliced at a valid boundary (the ASCII
// pad keeps the boundary clean), the empty string, and nonzero offsets.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    #[serial]
    fn prop_string_triple_equality(content in arb_string_content(), pad in arb_pad()) {
        let table = std_table();

        let text_v = text_repr_with_offset(content.as_bytes(), &pad);
        let list_v = charlist_repr(&content);
        let boxed_v = charlist_boxed_repr(&content);
        let lit_v = litstring_repr(&content);

        let r_text = String::from_value(&text_v, &table);
        let r_list = String::from_value(&list_v, &table);
        let r_boxed = String::from_value(&boxed_v, &table);
        let r_lit = String::from_value(&lit_v, &table);

        prop_assert!(r_text.is_ok(), "Text repr failed for {:?}: {:?}", content, r_text);
        prop_assert!(r_list.is_ok(), "[Char] repr failed for {:?}: {:?}", content, r_list);
        prop_assert!(r_boxed.is_ok(), "boxed [C# Char] repr failed for {:?}: {:?}", content, r_boxed);
        prop_assert!(r_lit.is_ok(), "LitString repr failed for {:?}: {:?}", content, r_lit);

        prop_assert_eq!(r_text.unwrap(), content.clone(), "Text decode != content");
        prop_assert_eq!(r_list.unwrap(), content.clone(), "[Char] decode != content");
        prop_assert_eq!(r_boxed.unwrap(), content.clone(), "boxed decode != content");
        prop_assert_eq!(r_lit.unwrap(), content.clone(), "LitString decode != content");
    }
}

// ===========================================================================
// PROPERTY 4: get_resilient collision resilience (x200).
//
// A DataConTable carrying multiple unqualified "I#" entries at DIFFERENT
// arities (arity-disambiguated). Both directions of the i64 bridge must pick
// the genuine arity-1 boxing constructor — never silently grab a same-name
// decoy at the wrong arity — or error cleanly.
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    #[serial]
    fn prop_get_resilient_collision(decoy_arity in 0u32..4u32, n in any::<i64>()) {
        let mut t = DataConTable::new();
        let mut add = |id: u64, name: &str, arity: u32| {
            t.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: vec![SrcBang::NoSrcBang; arity as usize],
                qualified_name: None,
            });
        };
        // A same-name "I#" decoy at a DIFFERENT arity, inserted FIRST.
        if decoy_arity != 1 {
            add(100, "I#", decoy_arity);
        }
        // The genuine boxing I# (arity 1).
        add(10, "I#", 1);
        // Another same-name decoy at arity 0, inserted LAST (probes scan order).
        add(101, "I#", 0);
        // `i64::to_value` needs a `Just`-less table no further; it only looks up I#/1.

        // ToCore side: must emit the arity-1 I# under collision.
        let boxed = n.to_value(&t).expect("i64::to_value under collision");
        if let Value::Con(id, _) = &boxed {
            prop_assert_eq!(
                t.get_by_name_arity("I#", 1),
                Some(*id),
                "ToCore picked a non-arity-1 I# under collision"
            );
        } else {
            prop_assert!(false, "i64::to_value produced a non-Con: {:?}", boxed);
        }

        // FromCore side: must round-trip the integer back.
        let decoded = i64::from_value(&boxed, &t);
        prop_assert!(decoded.is_ok(), "i64::from_value failed under collision: {:?}", decoded);
        prop_assert_eq!(decoded.unwrap(), n);

        // A bare literal decodes regardless of any name collisions.
        let bare = Value::Lit(Literal::LitInt(n));
        prop_assert_eq!(i64::from_value(&bare, &t).unwrap(), n);
    }
}

// ===========================================================================
// PROPERTY 5: cap-straddling crash hunt (forked).
//
// Field counts around MAX_FIELDS = 1024 and depths around MAX_DEPTH = 10_000.
// Each case runs in a forked child; a fatal signal is B3. The generators are
// weighted to land on the exact fenceposts well above the 5%-of-runs floor, and
// the reach counters confirm coverage.
// ===========================================================================

fn arb_wide_field_count() -> impl Strategy<Value = usize> {
    prop_oneof![
        3 => Just(1023usize),
        3 => Just(1024usize),
        3 => Just(1025usize),
        2 => 1000usize..=1030usize,
    ]
}

fn arb_deep_depth() -> impl Strategy<Value = usize> {
    prop_oneof![
        3 => Just(9999usize),
        3 => Just(10000usize),
        3 => Just(10001usize),
        2 => 9990usize..=10010usize,
    ]
}

#[cfg(unix)]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(80))]

    #[test]
    #[serial]
    fn prop_wide_con_capstraddle(n in arb_wide_field_count()) {
        FIELDS_TOTAL.fetch_add(1, Ordering::Relaxed);
        match n {
            1023 => { FIELDS_1023.fetch_add(1, Ordering::Relaxed); }
            1024 => { FIELDS_1024.fetch_add(1, Ordering::Relaxed); }
            1025 => { FIELDS_1025.fetch_add(1, Ordering::Relaxed); }
            _ => {}
        }
        let outcome = run_forked(move || {
            let fields: Vec<Value> = (0..n)
                .map(|i| Value::Lit(Literal::LitInt(i as i64)))
                .collect();
            let v = Value::Con(DataConId(2), fields);
            let nursery = estimate_nursery(&v);
            // Over MAX_FIELDS the decode must REFUSE, not silently succeed.
            // SAFETY: fresh nursery owned by the closure.
            let r = unsafe { heap_roundtrip(&v, nursery) };
            if n > 1024 && r.is_ok() {
                panic!("decode of {}-field Con (> MAX_FIELDS) unexpectedly succeeded", n);
            }
            if let Ok(d) = r {
                flatten_drop(d);
            }
            flatten_drop(v);
        });
        prop_assert!(
            outcome != ForkOutcome::ChildPanicked,
            "wide Con (n={}) child panicked (decode invariant violated)", n
        );
        if let ForkOutcome::Signaled(sig) = outcome {
            prop_assert!(false, "B3: wide Con (n={}) raised fatal signal {}", n, sig);
        }
    }
}

#[cfg(unix)]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(80))]

    #[test]
    #[serial]
    fn prop_deep_chain_capstraddle(depth in arb_deep_depth()) {
        DEPTH_TOTAL.fetch_add(1, Ordering::Relaxed);
        match depth {
            9999 => { DEPTH_9999.fetch_add(1, Ordering::Relaxed); }
            10000 => { DEPTH_10000.fetch_add(1, Ordering::Relaxed); }
            10001 => { DEPTH_10001.fetch_add(1, Ordering::Relaxed); }
            _ => {}
        }
        let outcome = run_forked(move || {
            let v = build_deep_chain(depth);
            let nursery = depth * 128 + 256 * 1024;
            // SAFETY: fresh nursery owned by the closure.
            let r = unsafe { heap_roundtrip(&v, nursery) };
            if let Ok(decoded) = r {
                flatten_drop(decoded);
            }
            flatten_drop(v);
        });
        prop_assert!(
            outcome != ForkOutcome::ChildPanicked,
            "deep chain (depth={}) child panicked", depth
        );
        if let ForkOutcome::Signaled(sig) = outcome {
            prop_assert!(false, "B3: deep chain (depth={}) raised fatal signal {}", depth, sig);
        }
    }
}

// ===========================================================================
// Deterministic cap-straddling coverage. Guarantees each exact fencepost is
// hit at least once (regardless of proptest sampling) and re-confirms the
// in-/over-cap decode behavior under fork containment.
// ===========================================================================

#[cfg(unix)]
#[test]
#[serial]
fn reach_counters_capstraddle_coverage() {
    for n in [1023usize, 1024usize, 1025usize] {
        let outcome = run_forked(move || {
            let fields: Vec<Value> = (0..n)
                .map(|i| Value::Lit(Literal::LitInt(i as i64)))
                .collect();
            let v = Value::Con(DataConId(2), fields);
            let nursery = estimate_nursery(&v);
            // SAFETY: fresh nursery owned by the closure.
            let r = unsafe { heap_roundtrip(&v, nursery) };
            // Decode caps at MAX_FIELDS = 1024; 1025 must NOT silently succeed.
            if n > 1024 && r.is_ok() {
                panic!("decode of {}-field Con unexpectedly succeeded", n);
            }
            if let Ok(d) = r {
                flatten_drop(d);
            }
            flatten_drop(v);
        });
        match n {
            1023 => FIELDS_1023.fetch_add(1, Ordering::Relaxed),
            1024 => FIELDS_1024.fetch_add(1, Ordering::Relaxed),
            1025 => FIELDS_1025.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
        FIELDS_TOTAL.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            outcome,
            ForkOutcome::ChildPanicked,
            "{n}-field Con: decode invariant violated"
        );
        if let ForkOutcome::Signaled(sig) = outcome {
            panic!("B3: {n}-field Con fatal signal {sig}");
        }
    }

    for depth in [9999usize, 10000usize, 10001usize] {
        let outcome = run_forked(move || {
            let v = build_deep_chain(depth);
            let nursery = depth * 128 + 256 * 1024;
            // SAFETY: fresh nursery owned by the closure.
            let r = unsafe { heap_roundtrip(&v, nursery) };
            if let Ok(d) = r {
                flatten_drop(d);
            }
            flatten_drop(v);
        });
        match depth {
            9999 => DEPTH_9999.fetch_add(1, Ordering::Relaxed),
            10000 => DEPTH_10000.fetch_add(1, Ordering::Relaxed),
            10001 => DEPTH_10001.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
        DEPTH_TOTAL.fetch_add(1, Ordering::Relaxed);
        if let ForkOutcome::Signaled(sig) = outcome {
            panic!("B3: deep chain depth={depth} fatal signal {sig}");
        }
    }

    // Each fencepost covered.
    assert!(
        FIELDS_1023.load(Ordering::Relaxed) >= 1,
        "1023 fields uncovered"
    );
    assert!(
        FIELDS_1024.load(Ordering::Relaxed) >= 1,
        "1024 fields uncovered"
    );
    assert!(
        FIELDS_1025.load(Ordering::Relaxed) >= 1,
        "1025 fields uncovered"
    );
    assert!(
        DEPTH_9999.load(Ordering::Relaxed) >= 1,
        "9999 depth uncovered"
    );
    assert!(
        DEPTH_10000.load(Ordering::Relaxed) >= 1,
        "10000 depth uncovered"
    );
    assert!(
        DEPTH_10001.load(Ordering::Relaxed) >= 1,
        "10001 depth uncovered"
    );

    // Cap-straddling fenceposts must make up >= 5% of all relevant runs. This
    // test only runs after the proptest cases have accumulated into the
    // counters when the whole file is run together; when run in isolation the
    // deterministic increments above still satisfy the floor.
    let f_fence = FIELDS_1023.load(Ordering::Relaxed)
        + FIELDS_1024.load(Ordering::Relaxed)
        + FIELDS_1025.load(Ordering::Relaxed);
    let f_total = FIELDS_TOTAL.load(Ordering::Relaxed).max(1);
    assert!(
        f_fence * 20 >= f_total,
        "wide-Con fencepost coverage {f_fence}/{f_total} below 5%"
    );
    let d_fence = DEPTH_9999.load(Ordering::Relaxed)
        + DEPTH_10000.load(Ordering::Relaxed)
        + DEPTH_10001.load(Ordering::Relaxed);
    let d_total = DEPTH_TOTAL.load(Ordering::Relaxed).max(1);
    assert!(
        d_fence * 20 >= d_total,
        "deep-chain fencepost coverage {d_fence}/{d_total} below 5%"
    );
}

// ===========================================================================
// FINDINGS — confirmed bugs. Each is `#[ignore]`d so the active suite stays
// GREEN; remove the `#[ignore]` to watch it fail (and reproduce). See
// `plans/proptest-findings-boundary.md` for the ledger.
// ===========================================================================

// ---------------------------------------------------------------------------
// B2 — `String::from_value` panics on a malformed `Text(ba, off, len)` whose
// `off`/`len` sum overflows `usize`, instead of returning a clean `Err`.
//
//   observed:  thread panics `attempt to add with overflow` (debug) /
//              `slice index starts at .. but ends at ..` (release) at
//              tidepool-bridge/src/impls.rs:390 — the bounds check
//              `if off + len > ba.len()` adds two `usize`s without overflow
//              guarding. A negative `off` field (`LitInt(-1)` decodes to
//              `usize::MAX` via `i64::from_value(..) as usize`) makes the sum
//              wrap/overflow before the comparison can reject it.
//   expected:  `Err(BridgeError::TypeMismatch { expected: "valid Text slice", .. })`
//              — the same clean rejection the non-overflowing huge-`len` path
//              already produces.
//   class:     B2 (decode crash on malformed input; not a fatal signal, a
//              caught panic; no memory unsafety — the slice start always
//              exceeds the end, so it is a guaranteed panic in both profiles,
//              never an out-of-bounds read).
//   component: tidepool-bridge/src/impls.rs:390 — `String::from_value`, Text
//              constructor arm.
//   seed:      minimal failing input off=-1, len=1 (ba = "hi"); proptest
//              regression seed committed under
//              tidepool-codegen/proptest-regressions/tests/proptest_boundary_roundtrip.txt
//              for `prop_text_offset_malformed_clean_err`.
//
// Real GHC `Text` never carries a negative offset, but the bridge is a trust
// boundary that decodes raw heap objects; the code clearly INTENDS to validate
// the slice (`if off + len > ba.len()`) and merely does the arithmetic
// unsafely. A `checked_add` (treating overflow as out-of-range) closes it.
// FIXED 2026-06-10: off/len are now validated as signed and combined with
// checked arithmetic in `String::from_value`; every malformed slice is a clean
// `Err(TypeMismatch)`. This is the active regression test.
#[test]
fn repro_b2_text_offset_overflow_panic() {
    let table = std_table();
    let v = Value::Con(
        DataConId(CON_TEXT),
        vec![
            Value::ByteArray(Arc::new(Mutex::new(b"hi".to_vec()))),
            Value::Lit(Literal::LitInt(-1)), // off -> usize::MAX
            Value::Lit(Literal::LitInt(1)),  // off + len overflows usize
        ],
    );
    // With the bug present this UNWINDS (caught here to show the diagnosis);
    // the assertion documents the contract that is currently violated.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        String::from_value(&v, &table)
    }));
    assert!(
        matches!(
            r,
            Ok(Err(
                tidepool_bridge::error::BridgeError::TypeMismatch { .. }
            ))
        ),
        "expected clean Err(TypeMismatch) for overflowing Text slice, got {:?}",
        r.as_ref().map(|inner| inner.as_ref().map(|_| "Ok(String)"))
    );
}

// ---------------------------------------------------------------------------
// B5 — `value_to_heap` silently truncates a `Con`'s field count to 16 bits,
// so a `Con` with exactly 2^16 + k fields (k <= MAX_FIELDS) round-trips to a
// SMALLER `Con` with no error.
//
//   observed:  a 65536-field `Con` encodes with the header `num_fields` field
//              (a u16 at CON_NUM_FIELDS_OFFSET) written as
//              `field_ptrs.len() as u16` = 0, and decodes back to a 0-field
//              `Con` — silent data loss. The decode-side `MAX_FIELDS = 1024`
//              guard inspects only the truncated header, so it never fires.
//              (Counts in 1025..=65535 are safe: they exceed u16 truncation
//              only past 65535, and decode cleanly rejects them with
//              `TooManyFields`. The corruption window is 65536..=66560, whose
//              truncated counts 0..=1024 all look in-bounds.)
//   expected:  either a clean encode error (the field count exceeds what the
//              layout can represent / the documented MAX_FIELDS), or a
//              round-trip-identical decode. Never a silently smaller `Con`.
//   class:     B5 (silent roundtrip non-identity / data corruption).
//   component: tidepool-codegen/src/heap_bridge.rs:402 — `value_to_heap`,
//              `*(.. CON_NUM_FIELDS_OFFSET ..) = field_ptrs.len() as u16`.
//   seed:      deterministic (n = 65536); no random input.
//
// This is far beyond the 1024 field cap of interest, but the ENCODE side has
// no cap at all and truncates instead of refusing, which is the defect. A
// `u16::try_from(len)` (erroring on overflow) on the write side closes it.
// FIXED 2026-06-10: the encode side now refuses Cons wider than MAX_FIELDS
// (the same bound the decode side enforces) with a clean
// `Err(TooManyFields)` instead of truncating the u16 header. This is the
// active regression test: silent truncation is impossible because nothing
// above the bound is ever written.
#[test]
fn repro_b5_con_field_count_u16_truncation() {
    let n = 65536usize; // 2^16 -> would truncate to 0 in the u16 header
    let fields: Vec<Value> = (0..n)
        .map(|i| Value::Lit(Literal::LitInt(i as i64)))
        .collect();
    let v = Value::Con(DataConId(2), fields);
    let (_nursery, mut vmctx) = setup_vmctx(n * 64 + 4 * 1024 * 1024);
    // SAFETY: nursery outlives vmctx.
    let encoded = unsafe { value_to_heap(&v, &mut vmctx) };
    flatten_drop(v);
    match encoded {
        Err(tidepool_codegen::heap_bridge::BridgeError::TooManyFields { count }) => {
            assert_eq!(count, n);
        }
        other => panic!(
            "B5: expected clean Err(TooManyFields) for a {n}-field Con, got {:?}",
            other.map(|_| "Ok(ptr)")
        ),
    }
}

// The hunting property that discovered B2. `#[ignore]`d so the active suite is
// GREEN; its committed `.proptest-regressions` seed pins the minimal failure.
// Remove `#[ignore]` to re-run the search (it will fail and re-shrink to
// off=-1, len=1).
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        // Integration tests have no nearby lib.rs/main.rs for proptest's default
        // SourceParallel anchor, so pin the regression file explicitly (path is
        // relative to the crate root = cargo's CWD when running tests).
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(
                "tests/proptest-regressions/proptest_boundary_roundtrip.txt",
            ),
        )),
        ..ProptestConfig::default()
    })]

    #[test]
    #[ignore = "BUG B2: hunting property for Text off+len overflow; seed committed"]
    #[serial]
    fn prop_text_offset_malformed_clean_err(off in -2i64..=2i64, len in 0i64..=3i64) {
        let table = std_table();
        let v = Value::Con(
            DataConId(CON_TEXT),
            vec![
                Value::ByteArray(Arc::new(Mutex::new(b"hi".to_vec()))),
                Value::Lit(Literal::LitInt(off)),
                Value::Lit(Literal::LitInt(len)),
            ],
        );
        // A correct decoder NEVER panics on a malformed slice — it returns Err.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            String::from_value(&v, &table)
        }));
        prop_assert!(
            r.is_ok(),
            "B2: String::from_value panicked on malformed Text(off={}, len={}) instead of returning Err",
            off, len
        );
    }
}
