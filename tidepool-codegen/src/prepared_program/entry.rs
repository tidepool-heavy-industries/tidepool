//! Generated thunk settlement shared by all prepared entry sites.

use cranelift_codegen::ir::{condcodes::IntCC, InstBuilder, MemFlags, Value};
use cranelift_codegen::{
    ir::{self, types, AbiParam},
    isa::CallConv,
    Context,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::{FuncId, Module};
use std::sync::Arc;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::execution_descriptor::{DescriptorState, FORWARDING_POINTER_OFFSET};
use tidepool_repr::execution_schema::UpdatePolicy;

/// A view of the compiled owner's callable metadata while definitions are
/// emitted. Function IDs are module relocations, not cast Rust function values.
pub(super) struct ThunkEntry {
    pub descriptor: Arc<ObjectDescriptor>,
    pub body: FuncId,
    pub policy: UpdatePolicy,
}

pub(super) fn signature() -> ir::Signature {
    let mut signature = ir::Signature::new(CallConv::Tail);
    signature.params = vec![AbiParam::new(types::I64); 2];
    signature.returns = vec![AbiParam::new(types::I32), AbiParam::new(types::I64)];
    signature
}

/// Emit the program's single lazy-entry state machine. Its input is a managed
/// reference produced by checked code (never an arbitrary host address).
/// Descriptor comparisons are centralized here; Enter sites call this entry.
/// Body calls and recursive result entry use the Tail convention without a
/// Rust bridge. The original thunk stays live in the stack map through both.
pub(super) fn emit_prepared_enter(
    pipeline: &mut crate::pipeline::CodegenPipeline,
    function: FuncId,
    thunks: &[ThunkEntry],
    evaluated: &[Arc<ObjectDescriptor>],
    poll: FuncId,
    stack_overflow: FuncId,
    bad_state: FuncId,
    blackhole: FuncId,
) -> Result<(), super::CompileError> {
    use crate::prepared_control::CallStatus;
    let mut context = Context::new();
    context.func.signature = signature();
    let mut frontend = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
    let start = builder.create_block();
    builder.append_block_params_for_function_params(start);
    builder.switch_to_block(start);
    builder.seal_block(start);
    let vmctx = builder.block_params(start)[0];
    let original = builder.block_params(start)[1];
    builder.declare_value_needs_stack_map(original);
    let loop_block = builder.create_block();
    builder.append_block_param(loop_block, types::I64);
    let invalid = builder.create_block();
    let return_value = builder.create_block();
    builder.append_block_param(return_value, types::I64);
    let poll_ref = pipeline.module.declare_func_in_func(poll, builder.func);
    let bad_ref = pipeline
        .module
        .declare_func_in_func(bad_state, builder.func);
    let blackhole_ref = pipeline
        .module
        .declare_func_in_func(blackhole, builder.func);
    let self_ref = pipeline.module.declare_func_in_func(function, builder.func);
    let preflight = super::emit::emit_preflight(&mut builder, vmctx, stack_overflow, pipeline);
    let early_abort = builder.create_block();
    let enough_stack = builder.ins().icmp_imm(IntCC::Equal, preflight, 0);
    builder.ins().brif(
        enough_stack,
        loop_block,
        &[original.into()],
        early_abort,
        &[],
    );
    builder.switch_to_block(early_abort);
    builder.seal_block(early_abort);
    crate::alloc::emit_prepared_failure_return(&mut builder, preflight);
    builder.switch_to_block(loop_block);
    let reference = builder.block_params(loop_block)[0];
    builder.declare_value_needs_stack_map(reference);
    let point = builder.ins().iconst(
        types::I32,
        crate::prepared_control::PreparedSafepoint::ThunkEntry as i64,
    );
    let poll_call = builder.ins().call(poll_ref, &[vmctx, point]);
    let status = builder.inst_results(poll_call)[0];
    let inspect = builder.create_block();
    let abort = builder.create_block();
    let ok = builder.ins().icmp_imm(IntCC::Equal, status, 0);
    builder.ins().brif(ok, inspect, &[], abort, &[]);
    builder.switch_to_block(abort);
    builder.seal_block(abort);
    crate::alloc::emit_prepared_failure_return(&mut builder, status);
    builder.switch_to_block(inspect);
    builder.seal_block(inspect);
    let untagged = builder.create_block();
    let tag = builder.ins().band_imm(reference, 7);
    let tagged = builder.ins().icmp_imm(IntCC::NotEqual, tag, 0);
    builder
        .ins()
        .brif(tagged, return_value, &[reference.into()], untagged, &[]);
    builder.switch_to_block(untagged);
    builder.seal_block(untagged);
    let read_header = builder.create_block();
    let null = builder.ins().icmp_imm(IntCC::Equal, reference, 0);
    builder.ins().brif(null, invalid, &[], read_header, &[]);
    builder.switch_to_block(read_header);
    builder.seal_block(read_header);
    let header = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), reference, 0);
    for descriptor in evaluated {
        let next = builder.create_block();
        let matches = builder.ins().icmp_imm(
            IntCC::Equal,
            header,
            descriptor.initial_header_word() as i64,
        );
        builder
            .ins()
            .brif(matches, return_value, &[reference.into()], next, &[]);
        builder.switch_to_block(next);
        builder.seal_block(next);
    }
    for thunk in thunks {
        let next = builder.create_block();
        let states = builder.create_block();
        let descriptor_word = thunk.descriptor.initial_header_word() as i64;
        let descriptor = builder.ins().band_imm(header, !7_i64);
        let matches = builder
            .ins()
            .icmp_imm(IntCC::Equal, descriptor, descriptor_word);
        builder.ins().brif(matches, states, &[], next, &[]);
        builder.switch_to_block(states);
        builder.seal_block(states);
        let state = builder.ins().band_imm(header, 7);
        let updated = builder.create_block();
        let not_updated = builder.create_block();
        let is_updated =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, state, DescriptorState::Updated as i64);
        builder
            .ins()
            .brif(is_updated, updated, &[], not_updated, &[]);
        builder.switch_to_block(updated);
        builder.seal_block(updated);
        let target = builder.ins().load(
            types::I64,
            MemFlags::trusted(),
            reference,
            FORWARDING_POINTER_OFFSET as i32,
        );
        builder.ins().jump(loop_block, &[target.into()]);
        builder.switch_to_block(not_updated);
        builder.seal_block(not_updated);
        let blackholed = builder.create_block();
        let not_blackholed = builder.create_block();
        let is_evaluating =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, state, DescriptorState::Evaluating as i64);
        builder
            .ins()
            .brif(is_evaluating, blackholed, &[], not_blackholed, &[]);
        builder.switch_to_block(blackholed);
        builder.seal_block(blackholed);
        let failure = builder.ins().call(blackhole_ref, &[vmctx]);
        let status = builder.inst_results(failure)[0];
        crate::alloc::emit_prepared_failure_return(&mut builder, status);
        builder.switch_to_block(not_blackholed);
        builder.seal_block(not_blackholed);
        let enter = builder.create_block();
        let live = builder
            .ins()
            .icmp_imm(IntCC::Equal, state, DescriptorState::Live as i64);
        builder.ins().brif(live, enter, &[], invalid, &[]);
        builder.switch_to_block(enter);
        builder.seal_block(enter);
        let live_header = builder.ins().iconst(types::I64, descriptor_word);
        let evaluating = builder
            .ins()
            .bor_imm(live_header, DescriptorState::Evaluating as i64);
        builder
            .ins()
            .store(MemFlags::trusted(), evaluating, reference, 0);
        let body_ref = pipeline
            .module
            .declare_func_in_func(thunk.body, builder.func);
        let call = builder.ins().call(body_ref, &[vmctx, reference]);
        let returned = builder.inst_results(call).to_vec();
        let body_ok = builder.create_block();
        let settle = builder.create_block();
        builder.append_block_param(settle, types::I32);
        builder.append_block_param(settle, types::I64);
        let succeeded =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, returned[0], CallStatus::Success as i64);
        builder.ins().brif(
            succeeded,
            body_ok,
            &[],
            settle,
            &[returned[0].into(), returned[1].into()],
        );
        builder.switch_to_block(body_ok);
        builder.seal_block(body_ok);
        builder.declare_value_needs_stack_map(returned[1]);
        let forced = builder.ins().call(self_ref, &[vmctx, returned[1]]);
        let forced_results = builder.inst_results(forced).to_vec();
        let force_ok = builder.create_block();
        let force_succeeded =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, forced_results[0], CallStatus::Success as i64);
        builder.ins().brif(
            force_succeeded,
            force_ok,
            &[],
            settle,
            &[forced_results[0].into(), forced_results[1].into()],
        );
        builder.switch_to_block(force_ok);
        builder.seal_block(force_ok);
        builder.declare_value_needs_stack_map(forced_results[1]);
        let point = builder.ins().iconst(
            types::I32,
            crate::prepared_control::PreparedSafepoint::ThunkCommit as i64,
        );
        let final_poll = builder.ins().call(poll_ref, &[vmctx, point]);
        let final_status = builder.inst_results(final_poll)[0];
        builder
            .ins()
            .jump(settle, &[final_status.into(), forced_results[1].into()]);
        builder.switch_to_block(settle);
        builder.seal_block(settle);
        let status = builder.block_params(settle)[0];
        let result = builder.block_params(settle)[1];
        emit_thunk_completion(
            &mut builder,
            reference,
            live_header,
            status,
            result,
            thunk.policy,
        );
        builder.switch_to_block(next);
        builder.seal_block(next);
    }
    builder.ins().jump(invalid, &[]);
    builder.switch_to_block(invalid);
    builder.seal_block(invalid);
    let failure = builder.ins().call(bad_ref, &[vmctx]);
    let status = builder.inst_results(failure)[0];
    crate::alloc::emit_prepared_failure_return(&mut builder, status);
    builder.switch_to_block(return_value);
    builder.seal_block(return_value);
    let value = builder.block_params(return_value)[0];
    let success = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[success, value]);
    builder.seal_block(loop_block);
    builder.finalize();
    pipeline.define_function(function, &mut context)?;
    Ok(())
}

/// Finish a nursery thunk obligation after the body and final cancellation
/// sample. `thunk` is a declared managed SSA root reloaded by the stack-map
/// integration after every collecting call; `live_header` is its pinned
/// descriptor identity. The successful result is admitted evaluated evidence.
/// No safepoint may split payload/header publication. Reusable failures leave
/// captures intact; integrity failure must not dereference the heap at all.
/// Old-space update publication must add the owning noncollecting barrier here
/// before old thunks become admissible, not at individual Enter call sites.
pub(super) fn emit_thunk_completion(
    builder: &mut FunctionBuilder<'_>,
    thunk: Value,
    live_header: Value,
    status: Value,
    result: Value,
    policy: UpdatePolicy,
) {
    use crate::prepared_control::CallStatus;
    let success = builder.create_block();
    let failed = builder.create_block();
    let restore = builder.create_block();
    let unwind = builder.create_block();
    let ok = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, CallStatus::Success as i64);
    builder.ins().brif(ok, success, &[], failed, &[]);

    builder.switch_to_block(success);
    builder.seal_block(success);
    if policy == UpdatePolicy::Memoize {
        builder.ins().store(
            MemFlags::trusted(),
            result,
            thunk,
            FORWARDING_POINTER_OFFSET as i32,
        );
        let updated = builder
            .ins()
            .bor_imm(live_header, DescriptorState::Updated as i64);
        builder.ins().store(MemFlags::trusted(), updated, thunk, 0);
    }
    builder.ins().return_(&[status, result]);

    builder.switch_to_block(failed);
    builder.seal_block(failed);
    let terminal =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, status, CallStatus::IntegrityFailure as i64);
    builder.ins().brif(terminal, unwind, &[], restore, &[]);

    builder.switch_to_block(restore);
    builder.seal_block(restore);
    builder
        .ins()
        .store(MemFlags::trusted(), live_header, thunk, 0);
    builder.ins().jump(unwind, &[]);

    builder.switch_to_block(unwind);
    builder.seal_block(unwind);
    crate::alloc::emit_prepared_failure_return(builder, status);
}
