//! Bare-JIT compile-and-run harness for codegen-level tests.
//!
//! Drives `CodegenPipeline` + `VMContext` directly (no effect machine, no
//! Haskell frontend) — the level below [`crate::eval_harness::EvalHarness`].
//! Codegen `emit_*` integration tests share this instead of each carrying a
//! private copy of the pipeline/nursery/vmctx setup.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::machine_state::MachineState;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::CoreExpr;

/// A finished JIT run: the raw result plus everything that must stay alive
/// for the result pointer to remain valid (nursery, pipeline, machine state).
pub struct JitRun {
    pub result_ptr: *const u8,
    pub vmctx: VMContext,
    _nursery: Vec<u8>,
    _pipeline: CodegenPipeline,
    // Boxed so its address stays stable across the move out of
    // `compile_and_run` — `vmctx.machine_state` points at its heap allocation.
    _machine_state: Box<MachineState>,
}

impl JitRun {
    /// Force a heap pointer (resolve thunks to WHNF).
    ///
    /// # Safety
    /// `ptr` must point into this run's heap.
    pub unsafe fn force(&mut self, ptr: *const u8) -> *const u8 {
        host_fns::heap_force(&mut self.vmctx, ptr as *mut u8) as *const u8
    }
}

/// Set up pipeline + `nursery_size`-byte nursery, compile `tree`, call it,
/// and return the result with its backing state kept alive.
pub fn compile_and_run(tree: &CoreExpr, nursery_size: usize) -> JitRun {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = compile_expr(&mut pipeline, tree, "test_fn", &ExternalEnv::new())
        .expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; nursery_size];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);
    let machine_state = Box::new(MachineState::new());
    vmctx.machine_state = machine_state.as_ref() as *const MachineState as *mut MachineState;

    machine_state.set_gc_state(start, nursery.len());
    machine_state.set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    JitRun {
        result_ptr: result as *const u8,
        vmctx,
        _nursery: nursery,
        _pipeline: pipeline,
        _machine_state: machine_state,
    }
}

/// Read the i64 payload of a `LitObject`.
///
/// # Safety
/// `ptr` must point at a live heap object.
pub unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    *(ptr.add(16) as *const i64)
}

/// Read the f64 payload of a `LitObject`.
///
/// # Safety
/// `ptr` must point at a live heap object.
pub unsafe fn read_lit_double(ptr: *const u8) -> f64 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    f64::from_bits(*(ptr.add(16) as *const u64))
}

/// Read the f32 payload of a `LitObject`.
///
/// # Safety
/// `ptr` must point at a live heap object.
pub unsafe fn read_lit_float(ptr: *const u8) -> f32 {
    assert_eq!(layout::read_tag(ptr), layout::TAG_LIT);
    f32::from_bits(*(ptr.add(16) as *const u32))
}
