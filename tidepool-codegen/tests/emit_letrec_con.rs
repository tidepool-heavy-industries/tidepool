//! Tests for LetRec with Con (constructor) RHS bindings.
//!
//! GHC's freer-simple compilation produces LetRec groups where some bindings
//! are constructors (building continuation trees) alongside lambda bindings.
//! The codegen must pre-allocate both Con and Lam objects, bind them in env,
//! then fill fields/code-ptrs in a second pass.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::*;

struct TestResult {
    result_ptr: *const u8,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
}

fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = compile_expr(&mut pipeline, tree, "test_fn").expect("compile_expr failed");
    pipeline.finalize();

    let mut nursery = vec![0u8; 65536];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    TestResult {
        result_ptr: result as *const u8,
        _nursery: nursery,
        _pipeline: pipeline,
    }
}

unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT, "expected TAG_LIT");
    *(ptr.add(16) as *const i64)
}

unsafe fn read_con_tag(ptr: *const u8) -> u64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_CON, "expected TAG_CON");
    *(ptr.add(8) as *const u64)
}

unsafe fn read_con_field(ptr: *const u8, i: usize) -> *const u8 {
    *(ptr.add(24 + 8 * i) as *const *const u8)
}

// ---------------------------------------------------------------------------
// Test 1: LetRec with a single Con RHS
//
//   let rec x = Con_7(42#)
//   in x
//
// Verifies: Con RHS compiles, fields are filled, result is correct.
// ---------------------------------------------------------------------------
#[test]
fn test_letrec_single_con() {
    let x = VarId(1);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)),              // 0: field value
            CoreFrame::Con { tag: DataConId(7), fields: vec![0] }, // 1: Con_7(42#)
            CoreFrame::Var(x),                                 // 2: x
            CoreFrame::LetRec {                                // 3: root
                bindings: vec![(x, 1)],
                body: 2,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_con_tag(result.result_ptr), 7);
        let f0 = read_con_field(result.result_ptr, 0);
        assert_eq!(read_lit_int(f0), 42);
    }
}

// ---------------------------------------------------------------------------
// Test 2: LetRec with Con referencing another Con binding (mutual data)
//
//   let rec a = Con_1(10#)
//              b = Con_2(a)
//   in b
//
// Verifies: Con fields can reference other LetRec binders.
// ---------------------------------------------------------------------------
#[test]
fn test_letrec_con_mutual_reference() {
    let a = VarId(1);
    let b = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(10)),               // 0
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] }, // 1: a = Con_1(10#)
            CoreFrame::Var(a),                                  // 2: ref to a
            CoreFrame::Con { tag: DataConId(2), fields: vec![2] }, // 3: b = Con_2(a)
            CoreFrame::Var(b),                                  // 4: ref to b
            CoreFrame::LetRec {                                 // 5: root
                bindings: vec![(a, 1), (b, 3)],
                body: 4,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        // b = Con_2(a)
        assert_eq!(read_con_tag(result.result_ptr), 2);
        let inner = read_con_field(result.result_ptr, 0);
        // a = Con_1(10#)
        assert_eq!(read_con_tag(inner), 1);
        let val = read_con_field(inner, 0);
        assert_eq!(read_lit_int(val), 10);
    }
}

// ---------------------------------------------------------------------------
// Test 3: LetRec with mixed Con + Lam bindings
//
//   let rec f = λx. x
//              node = Con_5(f)
//   in node
//
// Verifies: Con fields can reference Lam closure pointers.
// The Con's field[0] should be a valid closure with TAG_CLOSURE.
// ---------------------------------------------------------------------------
#[test]
fn test_letrec_mixed_con_and_lam() {
    let f = VarId(1);
    let node = VarId(2);
    let x = VarId(3);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(x),                                  // 0: x
            CoreFrame::Lam { binder: x, body: 0 },             // 1: f = λx. x
            CoreFrame::Var(f),                                  // 2: ref to f
            CoreFrame::Con { tag: DataConId(5), fields: vec![2] }, // 3: node = Con_5(f)
            CoreFrame::Var(node),                               // 4: ref to node
            CoreFrame::LetRec {                                 // 5: root
                bindings: vec![(f, 1), (node, 3)],
                body: 4,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        // node = Con_5(f)
        assert_eq!(read_con_tag(result.result_ptr), 5);
        let closure_ptr = read_con_field(result.result_ptr, 0);
        // f should be a valid closure
        assert_eq!(
            layout::read_tag(closure_ptr),
            layout::TAG_CLOSURE,
            "Con field should point to a closure"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 4: LetRec with mixed Con + Lam, then CALL the closure from the Con
//
//   let rec f = λx. x + 1
//              leaf = Con_5(f)
//   in case leaf of Con_5 g -> g 41
//
// This mimics the freer-simple pattern: Leaf(continuation_closure).
// Verifies the closure stored in the Con has a valid code pointer.
// ---------------------------------------------------------------------------
#[test]
fn test_letrec_con_closure_is_callable() {
    let f = VarId(1);
    let leaf = VarId(2);
    let x = VarId(3);
    let g = VarId(4);
    let scrut = VarId(5);
    let tree = RecursiveTree {
        nodes: vec![
            // f = λx. x + 1
            CoreFrame::Var(x),                                              // 0
            CoreFrame::Lit(Literal::LitInt(1)),                             // 1
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: x + 1
            CoreFrame::Lam { binder: x, body: 2 },                         // 3: f = λx. x+1
            // leaf = Con_5(f)
            CoreFrame::Var(f),                                              // 4
            CoreFrame::Con { tag: DataConId(5), fields: vec![4] },          // 5: leaf = Con_5(f)
            // body: case leaf of Con_5 g -> g 41
            CoreFrame::Var(leaf),                                           // 6: scrutinee
            CoreFrame::Var(g),                                              // 7: g
            CoreFrame::Lit(Literal::LitInt(41)),                            // 8: 41
            CoreFrame::App { fun: 7, arg: 8 },                             // 9: g 41
            CoreFrame::Case {                                               // 10: case
                scrutinee: 6,
                binder: scrut,
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(5)),
                    binders: vec![g],
                    body: 9,
                }],
            },
            CoreFrame::LetRec {                                             // 11: root
                bindings: vec![(f, 3), (leaf, 5)],
                body: 10,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 42, "g(41) should be 41 + 1 = 42");
    }
}

// ---------------------------------------------------------------------------
// Test 5: LetRec with Con referencing a Lam that captures another LetRec binder
//
//   let rec base = 100#        (Con wrapping a lit — Con_1(100#))
//              f = λx. case base of Con_1 n -> x + n
//              wrapper = Con_2(f)
//   in case wrapper of Con_2 g -> g 5
//
// Verifies closures in Con nodes correctly capture other LetRec binders.
// ---------------------------------------------------------------------------
#[test]
fn test_letrec_con_lam_captures_sibling() {
    let base = VarId(1);
    let f = VarId(2);
    let wrapper = VarId(3);
    let x = VarId(4);
    let n = VarId(5);
    let g = VarId(6);
    let scrut1 = VarId(7);
    let scrut2 = VarId(8);

    let tree = RecursiveTree {
        nodes: vec![
            // base = Con_1(100#)
            CoreFrame::Lit(Literal::LitInt(100)),                            // 0
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] },           // 1: base

            // f = λx. case base of Con_1 n -> x + n
            CoreFrame::Var(base),                                            // 2: base ref
            CoreFrame::Var(x),                                               // 3: x
            CoreFrame::Var(n),                                               // 4: n
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![3, 4] },  // 5: x + n
            CoreFrame::Case {                                                // 6: case base
                scrutinee: 2,
                binder: scrut1,
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![n],
                    body: 5,
                }],
            },
            CoreFrame::Lam { binder: x, body: 6 },                          // 7: f

            // wrapper = Con_2(f)
            CoreFrame::Var(f),                                               // 8
            CoreFrame::Con { tag: DataConId(2), fields: vec![8] },           // 9: wrapper

            // body: case wrapper of Con_2 g -> g 5
            CoreFrame::Var(wrapper),                                         // 10
            CoreFrame::Var(g),                                               // 11
            CoreFrame::Lit(Literal::LitInt(5)),                              // 12
            CoreFrame::App { fun: 11, arg: 12 },                             // 13: g 5
            CoreFrame::Case {                                                // 14
                scrutinee: 10,
                binder: scrut2,
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(2)),
                    binders: vec![g],
                    body: 13,
                }],
            },

            CoreFrame::LetRec {                                              // 15: root
                bindings: vec![(base, 1), (f, 7), (wrapper, 9)],
                body: 14,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        assert_eq!(read_lit_int(result.result_ptr), 105, "g(5) = 5 + 100 = 105");
    }
}

// ---------------------------------------------------------------------------
// Test 6: LetRec with 3 Con bindings (mimics freer-simple continuation chain)
//
//   let rec leaf1 = Con_LEAF(f)       -- Leaf(f)
//              leaf2 = Con_LEAF(g)    -- Leaf(g)
//              node  = Con_NODE(leaf1, leaf2)  -- Node(leaf1, leaf2)
//              f = λx. x + 1
//              g = λx. x * 2
//   in node
//
// Verifies the exact pattern from the tide repl Core: multiple Con bindings
// referencing each other and Lam bindings in a single LetRec group.
// ---------------------------------------------------------------------------
#[test]
fn test_letrec_continuation_chain_structure() {
    let leaf_tag = DataConId(10);
    let node_tag = DataConId(20);

    let f = VarId(1);
    let g = VarId(2);
    let leaf1 = VarId(3);
    let leaf2 = VarId(4);
    let node = VarId(5);
    let x = VarId(6);

    let tree = RecursiveTree {
        nodes: vec![
            // f = λx. x + 1
            CoreFrame::Var(x),                                              // 0
            CoreFrame::Lit(Literal::LitInt(1)),                             // 1
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2
            CoreFrame::Lam { binder: x, body: 2 },                         // 3: f

            // g = λx. x * 2
            CoreFrame::Var(x),                                              // 4
            CoreFrame::Lit(Literal::LitInt(2)),                             // 5
            CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![4, 5] }, // 6
            CoreFrame::Lam { binder: x, body: 6 },                         // 7: g

            // leaf1 = Con_LEAF(f)
            CoreFrame::Var(f),                                              // 8
            CoreFrame::Con { tag: leaf_tag, fields: vec![8] },              // 9: leaf1

            // leaf2 = Con_LEAF(g)
            CoreFrame::Var(g),                                              // 10
            CoreFrame::Con { tag: leaf_tag, fields: vec![10] },             // 11: leaf2

            // node = Con_NODE(leaf1, leaf2)
            CoreFrame::Var(leaf1),                                          // 12
            CoreFrame::Var(leaf2),                                          // 13
            CoreFrame::Con { tag: node_tag, fields: vec![12, 13] },         // 14: node

            // body: node
            CoreFrame::Var(node),                                           // 15

            CoreFrame::LetRec {                                             // 16: root
                bindings: vec![
                    (f, 3),
                    (g, 7),
                    (leaf1, 9),
                    (leaf2, 11),
                    (node, 14),
                ],
                body: 15,
            },
        ],
    };
    let result = compile_and_run(&tree);
    unsafe {
        // node = Con_NODE(leaf1, leaf2)
        assert_eq!(read_con_tag(result.result_ptr), 20);

        let l1 = read_con_field(result.result_ptr, 0);
        assert_eq!(read_con_tag(l1), 10); // leaf1 = Con_LEAF(f)
        let f_closure = read_con_field(l1, 0);
        assert_eq!(layout::read_tag(f_closure), layout::TAG_CLOSURE);

        let l2 = read_con_field(result.result_ptr, 1);
        assert_eq!(read_con_tag(l2), 10); // leaf2 = Con_LEAF(g)
        let g_closure = read_con_field(l2, 0);
        assert_eq!(layout::read_tag(g_closure), layout::TAG_CLOSURE);
    }
}

// ---------------------------------------------------------------------------
// Test 7: Compile the actual tide repl CBOR — just check it compiles
// ---------------------------------------------------------------------------
#[test]
fn test_compile_repl_cbor() {
    // The repl Core is 1885 nodes deep — emit_node recurses and overflows
    // the default 8MB stack. Run with a larger stack.
    let result = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024) // 32 MB
        .spawn(compile_repl_cbor_inner)
        .unwrap()
        .join();
    match result {
        Ok(()) => {}
        Err(e) => std::panic::resume_unwind(e),
    }
}

fn compile_repl_cbor_inner() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../examples/tide/target/tidepool-cbor/Repl/repl.cbor"
    );
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("Skipping: repl.cbor not found (run cargo build -p tidepool-tide first)");
            return;
        }
    };
    let expr = tidepool_repr::serial::read::read_cbor(&data).unwrap();

    // Load meta for DataConTable
    let meta_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../examples/tide/target/tidepool-cbor/Repl/meta.cbor"
    );
    let meta_data = std::fs::read(meta_path).unwrap();
    let table = tidepool_repr::serial::read::read_metadata(&meta_data).unwrap();

    let expr = tidepool_codegen::datacon_env::wrap_with_datacon_env(&expr, &table);

    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let result = compile_expr(&mut pipeline, &expr, "repl");
    assert!(result.is_ok(), "compile_expr failed: {:?}", result.err());
    pipeline.finalize();
}
