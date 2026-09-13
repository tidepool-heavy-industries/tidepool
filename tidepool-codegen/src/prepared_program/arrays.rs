//! Descriptor-backed external arrays. Managed wrappers move; payload identities
//! remain owned by MachineState. No payload is allocated across a collecting call
//! until its wrapper has been reserved and its header initialized.

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Linkage, Module};
use tidepool_heap::{execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind};
use tidepool_repr::execution_schema::{OperationIdentity, ResultContract, RuntimeRep, Signature};

/// Only exact pinned-GHC signatures are admitted. Void remains in the logical
/// signature and disappears only at the native argument boundary.
#[derive(Clone, Copy)]
pub(super) enum ArrayOperation {
    NewBoxed,
}

pub(super) fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<ArrayOperation> {
    use RuntimeRep::*;
    if matches!(identity, OperationIdentity::PrimOp(name) if matches!(name.as_str(), "newSmallArray#" | "newArray#"))
        && signature.arguments == [Int(64), LiftedRef, Void]
        && signature.results == ResultContract::Returns(vec![Void, UnliftedRef])
    {
        Some(ArrayOperation::NewBoxed)
    } else {
        None
    }
}

/// Noncollecting allocation/initialization, called only after the wrapper bump.
/// The initial value is a generated, admitted managed reference. No cancellation
/// poll, GC, or callback may split payload creation from wrapper publication.
///
/// # Safety
/// vmctx and wrapper belong to the active invocation. The wrapper has a valid
/// external descriptor header and a writable word-8 payload slot.
pub(super) unsafe extern "C" fn prepared_new_boxed(
    vmctx: *mut crate::context::VMContext,
    wrapper: *mut u8,
    length: i64,
    initial: *mut u8,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let payload = usize::try_from(length).ok().and_then(|length| {
        machine.allocate_external_storage(ExternalStorageKind::BoxedArray, length).ok()
    });
    let Some(payload) = payload else {
        machine.set_first_cause(RuntimeError::HeapOverflow);
        return machine.prepared_call_status() as i32;
    };
    // Fresh Young storage cannot have an old-to-young edge. Initialization is
    // direct; subsequent stores must use the generation-aware owning API.
    let fields = unsafe { payload.add(8).cast::<*mut u8>() };
    for index in 0..length as usize {
        unsafe { fields.add(index).write(initial) };
    }
    unsafe { wrapper.add(8).cast::<*mut u8>().write(payload) };
    CallStatus::Success as i32
}

pub(super) fn emit_new_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: FuncId,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let initial = arguments[1];
    builder.declare_value_needs_stack_map(initial);
    let gc = pipeline.module.declare_func_in_func(gc, builder.func);
    let object = crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, descriptor, gc);
    let header = builder.ins().iconst(types::I64, descriptor.initial_header_word() as i64);
    builder.ins().store(MemFlags::trusted(), header, object, 0);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().store(MemFlags::trusted(), zero, object, 8);

    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 4];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline.module.declare_function("prepared_new_boxed", Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let call = builder.ins().call(host, &[vmctx, object, arguments[0], initial]);
    let status = builder.inst_results(call)[0];
    let success = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, status, 0);
    let valid = builder.create_block();
    let invalid = builder.create_block();
    builder.ins().brif(success, valid, &[], invalid, &[]);
    builder.switch_to_block(invalid);
    builder.seal_block(invalid);
    crate::alloc::emit_prepared_failure_return(builder, status);
    builder.switch_to_block(valid);
    builder.seal_block(valid);
    let result = builder.ins().bor_imm(object, 7);
    builder.declare_value_needs_stack_map(result);
    Ok(vec![result])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::*;

    /// W5_ARRAY_BOXED: extend this real-adapter fixture to write/read/GC, not a
    /// second array evaluator. The result is deliberately not a host array API.
    #[test]
    fn w5_array_boxed_allocation_survives_result_collection() {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::UnliftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::Void, RuntimeRep::UnliftedRef]),
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("Arrays", "C"),
            family: testing::identity("Arrays", "T"),
            host_id: tidepool_repr::DataConId(999),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![], strict_fields: vec![],
            layout: CheckedLayout { fields: vec![], alignment: 1, payload_size: 0, root_mask: vec![] },
        });
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Arrays", "initial"),
            binding: HeapBinding { id: ValueId(1), rhs: HeapRhs::Constructor { constructor: ConstructorId(0), fields: vec![] } },
        }));
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp("newSmallArray#".into()), signature: SignatureId(1),
        });
        wire.expressions.nodes = vec![
            ExprFrame::Operation { operation: OperationId(0), arguments: vec![
                Atom::Scalar(ScalarLiteral::Int { bits: 64, bytes: 3_i64.to_be_bytes().to_vec() }),
                Atom::Ref(ValueRef::Local(ValueId(1))), Atom::Void,
            ] },
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(4)))]),
            ExprFrame::Case {
                scrutinee: 0, binder: ValueId(2), kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Void, RuntimeRep::UnliftedRef]),
                alternatives: vec![Alternative { pattern: AlternativePattern::Default, binders: vec![ValueId(3), ValueId(4)], body: 1 }],
            },
        ];
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs { *body = 2; }
        }
        let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        let program = crate::prepared_program::CompiledProgram::compile(&linked).unwrap();
        let result = program.run_entry(ValueId(0), &[], &crate::prepared_program::RunOptions {
            collect_before_observation: true, ..Default::default()
        }, Arc::new(AtomicBool::new(false)));
        assert!(matches!(result, Err(crate::prepared_program::ExecutionError::Observation(
            crate::prepared_program::ObservationFailure::Unobservable(
                tidepool_heap::execution_descriptor::ObjectKind::External(ExternalStorageKind::BoxedArray)
            )
        ))));
    }
}
