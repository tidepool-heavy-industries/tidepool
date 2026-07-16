use tidepool_codegen::context::VMContext;
use tidepool_codegen::host_fns;
use tidepool_heap::layout;
use tidepool_repr::*;
use tidepool_testing::jit_run::{compile_and_run, read_lit_int, JitRun};

extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

#[test]
fn test_heap_force_on_evaluated_thunk() {
    unsafe {
        let mut nursery_u64 = vec![0u64; 128]; // 1024 bytes
        let start = nursery_u64.as_mut_ptr() as *mut u8;
        let end = start.add(1024);
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);

        // 1. Result object (Lit)
        let lit_ptr = start;
        layout::write_header(lit_ptr, layout::TAG_LIT, layout::LIT_SIZE as u32);
        *(lit_ptr.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit_ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 42;

        // 2. Already evaluated thunk pointing to that Lit
        let thunk_ptr = start.add(layout::LIT_SIZE);
        layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u32);
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
        let start = nursery_u64.as_mut_ptr() as *mut u8;
        let end = start.add(1024);
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);

        let lit_ptr = start;
        layout::write_header(lit_ptr, layout::TAG_LIT, layout::LIT_SIZE as u32);
        *(lit_ptr.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit_ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 100;

        let res = host_fns::heap_force(&mut vmctx, lit_ptr);
        assert_eq!(
            res, lit_ptr,
            "heap_force on Lit should return the pointer unchanged"
        );
        assert_eq!(read_lit_int(res), 100);
    }
}

#[test]
fn test_heap_force_on_con_object() {
    unsafe {
        let mut nursery_u64 = vec![0u64; 128]; // 1024 bytes
        let start = nursery_u64.as_mut_ptr() as *mut u8;
        let end = start.add(1024);
        let mut vmctx = VMContext::new(start, end, mock_gc_trigger);

        let con_ptr = start;
        let size = layout::CON_FIELDS_OFFSET;
        layout::write_header(con_ptr, layout::TAG_CON, size as u32);
        *(con_ptr.add(layout::CON_TAG_OFFSET) as *mut u64) = 7; // DataConId(7)
        *(con_ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 0;

        let res = host_fns::heap_force(&mut vmctx, con_ptr);
        assert_eq!(
            res, con_ptr,
            "heap_force on Con should return the pointer unchanged"
        );
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
            CoreFrame::Var(VarId(1)),           // 3
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 2,
                body: 3,
            }, // 4 (root)
        ],
    };

    let mut result: JitRun = compile_and_run(&tree, 65536);
    unsafe {
        // The result of LetNonRec might be a thunk if rhs was thunked.
        // But here body is just Var(x), so result_ptr should be the thunk or the value of x.
        let forced = result.force(result.result_ptr);
        assert_eq!(layout::read_tag(forced), layout::TAG_LIT);
        assert_eq!(read_lit_int(forced), 3);
    }
}
