use tidepool_codegen::context::VMContext;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_codegen::host_fns;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_repr::*;
use tidepool_heap::layout;

struct TestResult {
    result_ptr: *const u8,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
}

/// Helper: set up pipeline + nursery, compile expr, call it, return result ptr.
fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = compile_expr(&mut pipeline, tree, "test_fn").expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");
    
    let mut nursery = vec![0u8; 65536]; // 64KB nursery
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

/// Helper: read i64 value from a LitObject.
unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    *(ptr.add(16) as *const i64)
}

/// Helper: read con_tag from a ConObject.
unsafe fn read_con_tag(ptr: *const u8) -> u64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_CON);
    *(ptr.add(8) as *const u64)
}

#[test]
fn test_case_three_constructors() {
    // case Con(1, []) of { DataAlt(0) -> 10; DataAlt(1) -> 20; DataAlt(2) -> 30 }
    // With con_tag=1, should return 20
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Con { tag: DataConId(1), fields: vec![] },  // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(10)),                    // 1: alt 0 body
        CoreFrame::Lit(Literal::LitInt(20)),                    // 2: alt 1 body
        CoreFrame::Lit(Literal::LitInt(30)),                    // 3: alt 2 body
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(0)), binders: vec![], body: 1 },
                Alt { con: AltCon::DataAlt(DataConId(1)), binders: vec![], body: 2 },
                Alt { con: AltCon::DataAlt(DataConId(2)), binders: vec![], body: 3 },
            ],
        },                                                      // 4: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 20); }
}

#[test]
fn test_case_default_catches_unmatched() {
    // case Con(5, []) of { DataAlt(0) -> 10; Default -> 99 }
    // Con tag 5 doesn't match alt 0, so default fires
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Con { tag: DataConId(5), fields: vec![] },
        CoreFrame::Lit(Literal::LitInt(10)),
        CoreFrame::Lit(Literal::LitInt(99)),
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(0)), binders: vec![], body: 1 },
                Alt { con: AltCon::Default, binders: vec![], body: 2 },
            ],
        },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 99); }
}

#[test]
fn test_case_field_binding() {
    // case Con(0, [Lit(1), Lit(2)]) of { DataAlt(0) [a, b] -> a + b }
    // Should return 3
    let a = VarId(1);
    let b = VarId(2);
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(1)),                     // 0
        CoreFrame::Lit(Literal::LitInt(2)),                     // 1
        CoreFrame::Con { tag: DataConId(0), fields: vec![0, 1] }, // 2: scrutinee
        CoreFrame::Var(a),                                       // 3
        CoreFrame::Var(b),                                       // 4
        CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![3, 4] }, // 5: a + b
        CoreFrame::Case {
            scrutinee: 2,
            binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(0)), binders: vec![a, b], body: 5 },
            ],
        },                                                       // 6: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 3); }
}

#[test]
fn test_case_nested() {
    let x = VarId(1);
    let outer_binder = VarId(98);
    let inner_binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Con { tag: DataConId(1), fields: vec![] },   // 0: inner con
        CoreFrame::Con { tag: DataConId(0), fields: vec![0] },  // 1: outer con (scrutinee)
        CoreFrame::Lit(Literal::LitInt(42)),                     // 2: innermost body
        CoreFrame::Var(x),                                       // 3: reference to x for inner scrutinee
        CoreFrame::Case {                                        // 4: inner case
            scrutinee: 3,
            binder: inner_binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(1)), binders: vec![], body: 2 },
            ],
        },
        CoreFrame::Case {                                        // 5: outer case (root)
            scrutinee: 1,
            binder: outer_binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(0)), binders: vec![x], body: 4 },
            ],
        },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_case_lit_alt() {
    // case Lit(42) of { LitAlt(0) -> 10; LitAlt(42) -> 99; Default -> 0 }
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),                     // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(10)),                     // 1: alt 0 body
        CoreFrame::Lit(Literal::LitInt(99)),                     // 2: alt 42 body
        CoreFrame::Lit(Literal::LitInt(0)),                      // 3: default body
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitInt(0)), binders: vec![], body: 1 },
                Alt { con: AltCon::LitAlt(Literal::LitInt(42)), binders: vec![], body: 2 },
                Alt { con: AltCon::Default, binders: vec![], body: 3 },
            ],
        },                                                        // 4: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 99); }
}

#[test]
fn test_case_default_only() {
    // case Lit(42) of { Default -> 100 }
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),
        CoreFrame::Lit(Literal::LitInt(100)),
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::Default, binders: vec![], body: 1 },
            ],
        },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 100); }
}

#[test]
fn test_case_binder_used() {
    // case Con(0, [Lit(7)]) of x { DataAlt(0) [_] -> x }
    // Case binder x = the whole scrutinee Con object
    let x = VarId(1);
    let dummy = VarId(2);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(7)),                      // 0
        CoreFrame::Con { tag: DataConId(0), fields: vec![0] },  // 1: scrutinee
        CoreFrame::Var(x),                                       // 2: body uses case binder
        CoreFrame::Case {
            scrutinee: 1,
            binder: x,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(0)), binders: vec![dummy], body: 2 },
            ],
        },                                                        // 3: root
    ] };
    let result = compile_and_run(&tree);
    unsafe {
        // Result should be the Con object itself
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_CON);
        assert_eq!(read_con_tag(result.result_ptr), 0);
    }
}

#[test]
fn test_case_lit_double() {
    // case Lit(3.14) of { LitAlt(1.0) -> 10; LitAlt(3.14) -> 99; Default -> 0 }
    let binder = VarId(99);
    let pi_bits = 3.14f64.to_bits();
    let one_bits = 1.0f64.to_bits();
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitDouble(pi_bits)),                // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(10)),                         // 1: alt 1.0 body
        CoreFrame::Lit(Literal::LitInt(99)),                         // 2: alt 3.14 body
        CoreFrame::Lit(Literal::LitInt(0)),                          // 3: default body
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitDouble(one_bits)), binders: vec![], body: 1 },
                Alt { con: AltCon::LitAlt(Literal::LitDouble(pi_bits)), binders: vec![], body: 2 },
                Alt { con: AltCon::Default, binders: vec![], body: 3 },
            ],
        },                                                            // 4: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 99); }
}

#[test]
fn test_case_lit_float() {
    // case Lit(2.5f) of { LitAlt(1.0f) -> 10; LitAlt(2.5f) -> 77; Default -> 0 }
    let binder = VarId(99);
    // LitFloat stores f64 bits (GHC represents Float# as Double internally)
    let target_bits = 2.5f64.to_bits();
    let one_bits = 1.0f64.to_bits();
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitFloat(target_bits)),              // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(10)),                          // 1: alt 1.0 body
        CoreFrame::Lit(Literal::LitInt(77)),                          // 2: alt 2.5 body
        CoreFrame::Lit(Literal::LitInt(0)),                           // 3: default body
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitFloat(one_bits)), binders: vec![], body: 1 },
                Alt { con: AltCon::LitAlt(Literal::LitFloat(target_bits)), binders: vec![], body: 2 },
                Alt { con: AltCon::Default, binders: vec![], body: 3 },
            ],
        },                                                             // 4: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 77); }
}

#[test]
fn test_case_bool() {
    // case True of { True -> 1; False -> 0 }
    let true_id = DataConId(1);
    let false_id = DataConId(0);
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Con { tag: true_id, fields: vec![] },         // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(1)),                    // 1: True body
        CoreFrame::Lit(Literal::LitInt(0)),                    // 2: False body
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(true_id), binders: vec![], body: 1 },
                Alt { con: AltCon::DataAlt(false_id), binders: vec![], body: 2 },
            ],
        },                                                      // 3: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 1); }
}

#[test]
fn test_case_computed_int_compare() {
    // case (3 > 2) of { 1# -> 10; 0# -> 20 }
    // 3 > 2 returns 1# (True)
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(3)),                     // 0
        CoreFrame::Lit(Literal::LitInt(2)),                     // 1
        CoreFrame::PrimOp { op: PrimOpKind::IntGt, args: vec![0, 1] }, // 2: 3 > 2
        CoreFrame::Lit(Literal::LitInt(10)),                    // 3: 1# body
        CoreFrame::Lit(Literal::LitInt(20)),                    // 4: 0# body
        CoreFrame::Case {
            scrutinee: 2,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: 3 },
                Alt { con: AltCon::LitAlt(Literal::LitInt(0)), binders: vec![], body: 4 },
            ],
        },                                                      // 5: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 10); }
}

#[test]
fn test_case_many_lit_alts() {
    // case 2# of { 1# -> 10; 2# -> 20; 3# -> 30; DEFAULT -> 0 }
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(2)),                     // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(10)),                    // 1
        CoreFrame::Lit(Literal::LitInt(20)),                    // 2
        CoreFrame::Lit(Literal::LitInt(30)),                    // 3
        CoreFrame::Lit(Literal::LitInt(0)),                     // 4: default
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: 1 },
                Alt { con: AltCon::LitAlt(Literal::LitInt(2)), binders: vec![], body: 2 },
                Alt { con: AltCon::LitAlt(Literal::LitInt(3)), binders: vec![], body: 3 },
                Alt { con: AltCon::Default, binders: vec![], body: 4 },
            ],
        },                                                      // 5: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 20); }
}

#[test]
fn test_case_word_lit() {
    // case 0## of { 0## -> 100; DEFAULT -> 200 }
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitWord(0)),                    // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(100)),                   // 1
        CoreFrame::Lit(Literal::LitInt(200)),                   // 2: default
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitWord(0)), binders: vec![], body: 1 },
                Alt { con: AltCon::Default, binders: vec![], body: 2 },
            ],
        },                                                      // 3: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 100); }
}

#[test]
fn test_case_char_lit() {
    // case 'b'# of { 'a'# -> 1; 'b'# -> 2; DEFAULT -> 0 }
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitChar('b')),                  // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(1)),                     // 1
        CoreFrame::Lit(Literal::LitInt(2)),                     // 2
        CoreFrame::Lit(Literal::LitInt(0)),                     // 3: default
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitChar('a')), binders: vec![], body: 1 },
                Alt { con: AltCon::LitAlt(Literal::LitChar('b')), binders: vec![], body: 2 },
                Alt { con: AltCon::Default, binders: vec![], body: 3 },
            ],
        },                                                      // 4: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 2); }
}

#[test]
fn test_case_nested_field_binding() {
    // case Con(0, [Con(1, [42]), 99]) of { DataAlt(0) [a, b] -> case a of { DataAlt(1) [n] -> n } }
    let a = VarId(1);
    let b = VarId(2);
    let n = VarId(3);
    let outer_binder = VarId(98);
    let inner_binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(42)),                    // 0
        CoreFrame::Con { tag: DataConId(1), fields: vec![0] },  // 1: inner con
        CoreFrame::Lit(Literal::LitInt(99)),                    // 2
        CoreFrame::Con { tag: DataConId(0), fields: vec![1, 2] }, // 3: outer con (scrutinee)
        CoreFrame::Var(a),                                       // 4
        CoreFrame::Var(n),                                       // 5: body of inner case
        CoreFrame::Case {                                        // 6: inner case
            scrutinee: 4,
            binder: inner_binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(1)), binders: vec![n], body: 5 },
            ],
        },
        CoreFrame::Case {                                        // 7: outer case (root)
            scrutinee: 3,
            binder: outer_binder,
            alts: vec![
                Alt { con: AltCon::DataAlt(DataConId(0)), binders: vec![a, b], body: 6 },
            ],
        },
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_case_scrutinee_lambda_app() {
    // case ((\x -> x) 42) of { DEFAULT x -> x }
    let x = VarId(1);
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Var(x),                                       // 0: lambda body
        CoreFrame::Lam { binder: x, body: 0 },                  // 1: \x -> x
        CoreFrame::Lit(Literal::LitInt(42)),                    // 2: arg
        CoreFrame::App { fun: 1, arg: 2 },                      // 3: (\x -> x) 42 (scrutinee)
        CoreFrame::Var(binder),                                  // 4: body
        CoreFrame::Case {
            scrutinee: 3,
            binder,
            alts: vec![
                Alt { con: AltCon::Default, binders: vec![], body: 4 },
            ],
        },                                                      // 5: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}

#[test]
fn test_case_multiple_alts_same_result() {
    // case 2# of { 1# -> 42; 2# -> 42; DEFAULT -> 0 }
    let binder = VarId(99);
    let tree = RecursiveTree { nodes: vec![
        CoreFrame::Lit(Literal::LitInt(2)),                     // 0: scrutinee
        CoreFrame::Lit(Literal::LitInt(42)),                    // 1: shared result
        CoreFrame::Lit(Literal::LitInt(0)),                     // 2: default
        CoreFrame::Case {
            scrutinee: 0,
            binder,
            alts: vec![
                Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: 1 },
                Alt { con: AltCon::LitAlt(Literal::LitInt(2)), binders: vec![], body: 1 },
                Alt { con: AltCon::Default, binders: vec![], body: 2 },
            ],
        },                                                      // 3: root
    ] };
    let result = compile_and_run(&tree);
    unsafe { assert_eq!(read_lit_int(result.result_ptr), 42); }
}
