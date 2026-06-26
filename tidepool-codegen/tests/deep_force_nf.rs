//! Wave 1.B component K — the iterative, stack-safe heap `deep_force`-to-NF
//! primitive (`host_fns::deep_force`).
//!
//! `heap_force` only reaches WHNF (the outermost constructor); `deep_force`
//! drives the entire first-order (Tier-0) data spine to normal form with an
//! explicit work stack (no host recursion), so an arbitrarily deep structure
//! cannot overflow the host thread. Closures/PAPs (Tier-1) are NOT descended.
//!
//! These build heap objects by hand (no JIT) — the structures here are
//! thunk-free, so forcing is the identity on each node and the test isolates
//! the new behaviour: iterative descent + per-field write-back, and the
//! Tier-0/Tier-1 boundary. Thunk-forcing itself is `heap_force` (separately
//! covered); `deep_force` composes it.

use tidepool_codegen::host_fns;
use tidepool_codegen::layout;
use tidepool_heap::layout as heap_layout;

const FIELD_STRIDE: usize = 8;

/// Write a boxed `Int` Lit at `buf[offset..]`; return the next 8-aligned offset.
unsafe fn write_lit(buf: &mut [u8], offset: usize, value: i64) -> usize {
    let ptr = buf.as_mut_ptr().add(offset);
    heap_layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
    *ptr.add(layout::LIT_TAG_OFFSET as usize) = layout::LIT_TAG_INT as u8;
    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = value;
    offset + layout::LIT_TOTAL_SIZE as usize
}

/// Write a Con(`con_tag`, `fields`) at `buf[offset..]`; return next 8-aligned offset.
unsafe fn write_con(buf: &mut [u8], offset: usize, con_tag: u64, fields: &[*mut u8]) -> usize {
    let ptr = buf.as_mut_ptr().add(offset);
    let size = (layout::CON_FIELDS_OFFSET as usize + fields.len() * FIELD_STRIDE) as u16;
    let aligned = ((size as usize) + 7) & !7;
    heap_layout::write_header(ptr, layout::TAG_CON, size);
    *(ptr.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = con_tag;
    *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = fields.len() as u16;
    for (i, &f) in fields.iter().enumerate() {
        *(ptr.add(layout::CON_FIELDS_OFFSET as usize + i * FIELD_STRIDE) as *mut *mut u8) = f;
    }
    offset + aligned
}

/// Write a minimal Closure header (Tier-1). We never CALL it, so the code
/// pointer can be a sentinel — `deep_force` must force it to WHNF (identity for a
/// non-thunk) and NOT descend into it.
unsafe fn write_closure(buf: &mut [u8], offset: usize) -> usize {
    let ptr = buf.as_mut_ptr().add(offset);
    // size: header + code ptr + num_captured (no captures)
    let size = layout::CLOSURE_CAPTURED_OFFSET as u16;
    let aligned = ((size as usize) + 7) & !7;
    heap_layout::write_header(ptr, layout::TAG_CLOSURE, size);
    *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *mut usize) = 0xDEAD_BEEF;
    *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *mut u16) = 0;
    offset + aligned
}

/// A deeply-nested linked Con chain: `chain[i] = Con(1, [chain[i+1]])`,
/// terminated by `Lit(terminal)`. `deep_force` on the head must traverse the
/// whole chain WITHOUT host recursion — a recursive forcer would overflow the
/// thread stack at this depth — and return the (forced, here identical) head.
#[test]
fn deep_force_traverses_deep_chain_without_host_recursion() {
    const DEPTH: usize = 120_000;
    // Lit (24) + DEPTH Cons (32 each) + slack.
    let mut buf = vec![0u8; layout::LIT_TOTAL_SIZE as usize + DEPTH * 32 + 64];

    host_fns::clear_persistent_roots();
    host_fns::clear_rust_roots();

    unsafe {
        // Terminal Lit at offset 0.
        let next_off = write_lit(&mut buf, 0, 0xABCD);
        let mut child = buf.as_mut_ptr();
        let mut off = next_off;
        // Build Cons backward (deepest first) so each points at the prior object.
        for _ in 0..DEPTH {
            let here = buf.as_mut_ptr().add(off);
            off = write_con(&mut buf, off, 1, &[child]);
            child = here;
        }
        let head = child; // outermost Con

        // No GC state installed: heap_force on non-thunks is pure; register/
        // truncate roots are harmless no-ops without a collection.
        let forced = host_fns::deep_force(std::ptr::null_mut(), head);

        assert_eq!(forced, head, "thunk-free head forces to itself (identity)");
        assert_ne!(
            forced,
            host_fns::error_poison_ptr(),
            "must not poison on a well-formed structure"
        );

        // Walk the chain post-force: structure preserved, terminal intact.
        let mut cur = forced;
        let mut steps = 0usize;
        while heap_layout::read_tag(cur) == layout::TAG_CON {
            cur = *(cur.add(layout::CON_FIELDS_OFFSET as usize) as *const *mut u8);
            steps += 1;
        }
        assert_eq!(steps, DEPTH, "every Con link preserved after deep_force");
        assert_eq!(heap_layout::read_tag(cur), layout::TAG_LIT);
        assert_eq!(*(cur.add(layout::LIT_VALUE_OFFSET as usize) as *const i64), 0xABCD);
    }

    // Rust roots fully unwound (base_mark restored).
    assert_eq!(host_fns::rust_roots_mark(), 0, "deep_force must unwind all roots");
    host_fns::clear_persistent_roots();
}

/// `deep_force` forces a value to WHNF then descends into Tier-0 `Con` fields,
/// but treats a `Closure` field as Tier-1: it is forced to WHNF (identity) and
/// left as-is, NOT descended into. A bushy Con of [Lit, Closure, Con[Lit]]
/// exercises multiple fields + nesting.
#[test]
fn deep_force_descends_con_but_not_closure() {
    let mut buf = vec![0u8; 512];

    host_fns::clear_persistent_roots();
    host_fns::clear_rust_roots();

    unsafe {
        let mut off = 0;
        // inner Lit
        let inner_lit = buf.as_mut_ptr().add(off);
        off = write_lit(&mut buf, off, 7);
        // inner Con(2, [inner_lit])
        let inner_con = buf.as_mut_ptr().add(off);
        off = write_con(&mut buf, off, 2, &[inner_lit]);
        // a leaf Lit field
        let leaf_lit = buf.as_mut_ptr().add(off);
        off = write_lit(&mut buf, off, 99);
        // a Tier-1 closure field
        let clo = buf.as_mut_ptr().add(off);
        off = write_closure(&mut buf, off);
        // outer Con(1, [leaf_lit, clo, inner_con])
        let outer = buf.as_mut_ptr().add(off);
        let _ = write_con(&mut buf, off, 1, &[leaf_lit, clo, inner_con]);

        let forced = host_fns::deep_force(std::ptr::null_mut(), outer);
        assert_eq!(forced, outer);
        assert_ne!(forced, host_fns::error_poison_ptr());

        // Fields intact, types unchanged. The closure field is returned as-is
        // (forced to WHNF = identity) and was NOT mistaken for descendable data.
        let f0 = *(outer.add(layout::CON_FIELDS_OFFSET as usize) as *const *mut u8);
        let f1 = *(outer.add(layout::CON_FIELDS_OFFSET as usize + FIELD_STRIDE) as *const *mut u8);
        let f2 =
            *(outer.add(layout::CON_FIELDS_OFFSET as usize + 2 * FIELD_STRIDE) as *const *mut u8);
        assert_eq!(f0, leaf_lit);
        assert_eq!(heap_layout::read_tag(f0), layout::TAG_LIT);
        assert_eq!(f1, clo);
        assert_eq!(heap_layout::read_tag(f1), layout::TAG_CLOSURE, "closure left as-is");
        assert_eq!(f2, inner_con);
        // Nested Con's field forced/preserved.
        let nested =
            *(inner_con.add(layout::CON_FIELDS_OFFSET as usize) as *const *mut u8);
        assert_eq!(heap_layout::read_tag(nested), layout::TAG_LIT);
        assert_eq!(*(nested.add(layout::LIT_VALUE_OFFSET as usize) as *const i64), 7);
    }

    assert_eq!(host_fns::rust_roots_mark(), 0);
    host_fns::clear_persistent_roots();
}

/// A null root is returned unchanged (defensive — matches `heap_force`).
#[test]
fn deep_force_null_is_identity() {
    let r = host_fns::deep_force(std::ptr::null_mut(), std::ptr::null_mut());
    assert!(r.is_null());
}
