use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::*;

struct TestResult {
    result_ptr: *mut u8,
    vmctx: VMContext,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
}

impl TestResult {
    /// Force a heap pointer (resolve thunks to WHNF).
    unsafe fn force(&mut self, ptr: *mut u8) -> *mut u8 {
        host_fns::heap_force(&mut self.vmctx, ptr)
    }
}

/// Helper: set up pipeline + nursery, compile expr, call it, return result ptr.
fn compile_and_run(tree: &CoreExpr) -> TestResult {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = compile_expr(&mut pipeline, tree, "test_fn").expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 65536]; // 64KB nursery
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    host_fns::set_gc_state(start, nursery.len());
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    TestResult {
        result_ptr: result as *mut u8,
        vmctx,
        _nursery: nursery,
        _pipeline: pipeline,
    }
}

unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    *(ptr.add(layout::LIT_VALUE_OFFSET) as *const i64)
}

extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

#[test]
fn test_heap_force_on_evaluated_thunk() {
    unsafe {
        let mut nursery_u64 = vec![0u64; 128]; // 1024 bytes
        let nursery_ptr = nursery_u64.as_mut_ptr() as *mut u8;
        let start = nursery_ptr;
        let end = start.add(1024);
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);

        // 1. Result object (Lit)
        let lit_ptr = start;
        layout::write_header(lit_ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(lit_ptr.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit_ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 42;

        // 2. Already evaluated thunk pointing to that Lit
        let thunk_ptr = start.add(layout::LIT_SIZE);
        layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
        *(thunk_ptr.add(layout::THUNK_STATE_OFFSET)) = layout::THUNK_EVALUATED;
        *(thunk_ptr.add(layout::THUNK_INDIRECTION_OFFSET) as *mut *mut u8) = lit_ptr;

        let res = host_fns::heap_force(&mut vmctx, thunk_ptr);
        assert_eq!(res, lit_ptr);
        assert_eq!(read_lit_int(res), 42);
    }
}

#[test]
fn test_heap_force_on_lit_object() {
    unsafe {
        let mut nursery_u64 = vec![0u64; 128]; // 1024 bytes
        let nursery_ptr = nursery_u64.as_mut_ptr() as *mut u8;
        let start = nursery_ptr;
        let end = start.add(1024);
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);

        let lit_ptr = start;
        layout::write_header(lit_ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(lit_ptr.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit_ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 100;

        let res = host_fns::heap_force(&mut vmctx, lit_ptr);
        assert_eq!(res, lit_ptr, "heap_force on Lit should return the pointer unchanged");
        assert_eq!(read_lit_int(res), 100);
    }
}

#[test]
fn test_heap_force_on_con_object() {
    unsafe {
        let mut nursery_u64 = vec![0u64; 128]; // 1024 bytes
        let nursery_ptr = nursery_u64.as_mut_ptr() as *mut u8;
        let start = nursery_ptr;
        let end = start.add(1024);
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);

        let con_ptr = start;
        let size = layout::CON_FIELDS_OFFSET;
        layout::write_header(con_ptr, layout::TAG_CON, size as u16);
        *(con_ptr.add(layout::CON_TAG_OFFSET) as *mut u64) = 7; // DataConId(7)
        *(con_ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 0;

        let res = host_fns::heap_force(&mut vmctx, con_ptr);
        assert_eq!(res, con_ptr, "heap_force on Con should return the pointer unchanged");
        assert_eq!(layout::read_tag(res), layout::TAG_CON);
    }
}

#[test]
fn test_heap_force_thunk_evaluation() {
    // let x = 1 + 2 in x
    let tree = CoreExpr {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2
            CoreFrame::Var(VarId(1)), // 3
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 2,
                body: 3,
            }, // 4 (root)
        ],
    };
    
    let mut result = compile_and_run(&tree);
    unsafe {
        // The result of LetNonRec might be a thunk if rhs was thunked.
        // But here body is just Var(x), so result_ptr should be the thunk or the value of x.
        let forced = result.force(result.result_ptr);
        assert_eq!(layout::read_tag(forced), layout::TAG_LIT);
        assert_eq!(read_lit_int(forced), 3);
    }
}
