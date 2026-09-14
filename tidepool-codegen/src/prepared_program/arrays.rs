//! Descriptor-backed external arrays. Managed wrappers move; payload identities
//! remain owned by MachineState. No payload is allocated across a collecting call
//! until its wrapper has been reserved and its header initialized.

use super::primitives::returns_exact;
use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Linkage, Module};
use tidepool_heap::{
    descriptor_region::DescriptorOldSpace,
    execution_descriptor::{DescriptorState, ObjectDescriptor},
    external_storage::{ExternalStorageKind, ExternalStorageValidationError},
    managed_reference::untag,
};
use tidepool_repr::execution_schema::{OperationIdentity, RuntimeRep, Signature};

/// Only exact pinned-GHC signatures are admitted. Void remains in the logical
/// signature and disappears only at the native argument boundary.
#[derive(Clone, Copy)]
pub(super) enum ArrayOperation {
    NewBoxed,
    ReadBoxed,
    WriteBoxed,
    NewMutVar,
    ReadMutVar,
    WriteMutVar,
    SizeofBoxed,
    UnsafeFreezeBoxed,
    ShrinkSmallBoxed,
    CopyBoxed,
    CasBoxed,
}

#[cfg(test)]
#[test]
fn w5_bulk_mutable_copy_signature() {
    use tidepool_repr::execution_schema::ResultContract;
    use RuntimeRep::*;
    let signature = Signature {
        arguments: vec![UnliftedRef, Int(64), UnliftedRef, Int(64), Int(64), Void],
        results: ResultContract::Returns(vec![]),
    };
    for name in ["copyMutableArray#", "copySmallMutableArray#"] {
        assert!(recognize(&OperationIdentity::PrimOp(name.into()), &signature).is_some());
    }
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<ArrayOperation> {
    use RuntimeRep::*;
    let OperationIdentity::PrimOp(name) = identity else {
        return None;
    };
    match name.as_str() {
        "newSmallArray#" | "newArray#"
            if signature.arguments == [Int(64), LiftedRef, Void]
                && returns_exact(signature, &[UnliftedRef]) =>
        {
            Some(ArrayOperation::NewBoxed)
        }
        "readSmallArray#" | "readArray#"
            if signature.arguments == [UnliftedRef, Int(64), Void]
                && returns_exact(signature, &[LiftedRef]) =>
        {
            Some(ArrayOperation::ReadBoxed)
        }
        "indexSmallArray#" | "indexArray#"
            if signature.arguments == [UnliftedRef, Int(64)]
                && returns_exact(signature, &[LiftedRef]) =>
        {
            Some(ArrayOperation::ReadBoxed)
        }
        "writeSmallArray#" | "writeArray#"
            if signature.arguments == [UnliftedRef, Int(64), LiftedRef, Void]
                && returns_exact(signature, &[]) =>
        {
            Some(ArrayOperation::WriteBoxed)
        }
        "newMutVar#"
            if signature.arguments == [LiftedRef, Void]
                && returns_exact(signature, &[UnliftedRef]) =>
        {
            Some(ArrayOperation::NewMutVar)
        }
        "readMutVar#"
            if signature.arguments == [UnliftedRef, Void]
                && returns_exact(signature, &[LiftedRef]) =>
        {
            Some(ArrayOperation::ReadMutVar)
        }
        "writeMutVar#"
            if signature.arguments == [UnliftedRef, LiftedRef, Void]
                && returns_exact(signature, &[]) =>
        {
            Some(ArrayOperation::WriteMutVar)
        }
        "sizeofSmallArray#"
        | "sizeofSmallMutableArray#"
        | "sizeofArray#"
        | "sizeofMutableArray#"
            if signature.arguments == [UnliftedRef] && returns_exact(signature, &[Int(64)]) =>
        {
            Some(ArrayOperation::SizeofBoxed)
        }
        "unsafeFreezeSmallArray#" | "unsafeFreezeArray#"
            if signature.arguments == [UnliftedRef, Void]
                && returns_exact(signature, &[UnliftedRef]) =>
        {
            Some(ArrayOperation::UnsafeFreezeBoxed)
        }
        "shrinkSmallMutableArray#"
            if signature.arguments == [UnliftedRef, Int(64), Void]
                && returns_exact(signature, &[]) =>
        {
            Some(ArrayOperation::ShrinkSmallBoxed)
        }
        "copySmallMutableArray#" | "copyMutableArray#"
            if signature.arguments
                == [UnliftedRef, Int(64), UnliftedRef, Int(64), Int(64), Void]
                && returns_exact(signature, &[]) =>
        {
            Some(ArrayOperation::CopyBoxed)
        }
        "casSmallArray#" | "casArray#"
            if signature.arguments == [UnliftedRef, Int(64), LiftedRef, LiftedRef, Void]
                && returns_exact(signature, &[Int(64), LiftedRef]) =>
        {
            Some(ArrayOperation::CasBoxed)
        }
        _ => None,
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
        machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, length)
            .ok()
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

pub(super) fn array_error(
    machine: &crate::machine_state::MachineState,
    error: crate::host_fns::RuntimeError,
) -> i32 {
    machine.set_first_cause(error);
    machine.prepared_call_status() as i32
}

pub(super) fn storage_error(
    error: ExternalStorageValidationError,
    index: i64,
) -> crate::host_fns::RuntimeError {
    use crate::host_fns::RuntimeError;
    match error {
        ExternalStorageValidationError::IndexOutOfBounds { len, .. } => {
            RuntimeError::ArrayIndexOutOfBounds { index, len }
        }
        ExternalStorageValidationError::LengthIncrease { old, .. } => {
            RuntimeError::ArrayIndexOutOfBounds { index, len: old }
        }
        ExternalStorageValidationError::BookkeepingAllocation => RuntimeError::HeapOverflow,
        _ => RuntimeError::BadPointer,
    }
}

/// Authenticate a generated wrapper against its pinned descriptor before
/// reading its handle. The generated reference provides exact-start provenance;
/// nursery bounds or old-space admission and descriptor identity establish
/// that its storage is still live. No safepoint occurs during this borrow.
unsafe fn active_boxed_payload(
    machine: &crate::machine_state::MachineState,
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
) -> Result<(*mut u8, usize), crate::host_fns::RuntimeError> {
    unsafe {
        active_payload(
            machine,
            vmctx,
            reference,
            descriptor,
            ExternalStorageKind::BoxedArray,
        )
    }
}

/// Shared noncollecting wrapper admission for boxed and byte-array primitives.
/// Length comes from the authenticated ledger, never an unchecked prefix read.
/// The caller supplies the program-pinned descriptor and generated provenance.
pub(super) unsafe fn active_payload(
    machine: &crate::machine_state::MachineState,
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    kind: ExternalStorageKind,
) -> Result<(*mut u8, usize), crate::host_fns::RuntimeError> {
    use crate::host_fns::RuntimeError;
    if vmctx.is_null() || descriptor.is_null() || reference.is_null() {
        return Err(RuntimeError::BadPointer);
    }
    // SAFETY: the generated code embeds a pointer to its program-owned Arc.
    let descriptor = unsafe { &*descriptor };
    if descriptor.external_kind() != Some(kind)
        || (reference as usize & 7) != usize::from(descriptor.tag())
    {
        return Err(RuntimeError::BadPointer);
    }
    let encoded = reference as usize;
    let address = untag(encoded);
    let extent = descriptor.allocation_extent() as usize;
    let (start, size) = machine.gc_active_range().ok_or(RuntimeError::BadPointer)?;
    let active_end = (start as usize)
        .checked_add(size)
        .ok_or(RuntimeError::BadPointer)?;
    if address >= start as usize && address < active_end {
        let used_end = unsafe { (*vmctx).alloc_ptr } as usize;
        if used_end < start as usize
            || used_end > active_end
            || address.checked_add(extent).is_none_or(|end| end > used_end)
        {
            return Err(RuntimeError::BadPointer);
        }
    } else {
        let old = unsafe { machine.prepared_old_space() }.ok_or(RuntimeError::BadPointer)?;
        if old
            .admit(encoded)
            .map_err(|_| RuntimeError::BadPointer)?
            .is_none()
        {
            return Err(RuntimeError::BadPointer);
        }
    }
    let object = address as *mut u8;
    if unsafe { descriptor.state(object, extent) }.map_err(|_| RuntimeError::BadPointer)?
        != DescriptorState::Live
    {
        return Err(RuntimeError::BadPointer);
    }
    let handle = unsafe { descriptor.external_payload_slot(object, extent) }
        .map_err(|_| RuntimeError::BadPointer)?;
    let published = unsafe { handle.read() };
    let view = machine
        .external_active_view(published, kind)
        .map_err(|error| storage_error(error, 0))?;
    Ok((published, view.logical_len))
}

/// Read/index share the same authenticated, bounds-checked owner path. The
/// output is written only on success, so generated code cannot publish it
/// after a failed host call.
pub(super) unsafe extern "C" fn prepared_read_boxed(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    output: *mut *mut u8,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (published, len) =
            unsafe { active_boxed_payload(machine, vmctx, reference, descriptor) }?;
        let index = usize::try_from(index)
            .ok()
            .filter(|&index| index < len)
            .ok_or(RuntimeError::ArrayIndexOutOfBounds { index, len })?;
        // SAFETY: the active view proved the full aligned slot span and the
        // checked index places this load inside it.
        let slot = unsafe { published.add(8).cast::<*mut u8>().add(index) };
        unsafe { output.write(slot.read()) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_write_boxed(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    value: *mut u8,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (published, len) =
            unsafe { active_boxed_payload(machine, vmctx, reference, descriptor) }?;
        let index = usize::try_from(index)
            .ok()
            .filter(|&index| index < len)
            .ok_or(RuntimeError::ArrayIndexOutOfBounds { index, len })?;
        machine
            .store_external_element(published, index, value)
            .map_err(|error| storage_error(error, index as i64))
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_sizeof_boxed(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    output: *mut i64,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (_, len) = unsafe { active_boxed_payload(machine, vmctx, reference, descriptor) }?;
        let len = i64::try_from(len).map_err(|_| RuntimeError::BadPointer)?;
        unsafe { output.write(len) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_freeze_boxed(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
) -> i32 {
    use crate::prepared_control::CallStatus;
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    match unsafe { active_boxed_payload(machine, vmctx, reference, descriptor) } {
        Ok(_) => CallStatus::Success as i32,
        Err(error) => array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_shrink_boxed(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    new_len: i64,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (published, len) =
            unsafe { active_boxed_payload(machine, vmctx, reference, descriptor) }?;
        let new_len = usize::try_from(new_len)
            .ok()
            .filter(|&candidate| candidate <= len)
            .ok_or(RuntimeError::ArrayIndexOutOfBounds {
                index: new_len,
                len,
            })?;
        machine
            .shrink_external_payload(published, ExternalStorageKind::BoxedArray, new_len)
            .map_err(|error| storage_error(error, new_len as i64))
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => array_error(machine, error),
    }
}

fn checked_element_span(value: i64, len: usize) -> Result<usize, crate::host_fns::RuntimeError> {
    usize::try_from(value)
        .map_err(|_| crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: value, len })
}

fn boxed_range_error(error: ExternalStorageValidationError) -> crate::host_fns::RuntimeError {
    match error {
        ExternalStorageValidationError::IndexOutOfBounds { index, len } => {
            crate::host_fns::RuntimeError::ArrayIndexOutOfBounds {
                index: i64::try_from(index).unwrap_or(i64::MAX),
                len,
            }
        }
        other => storage_error(other, 0),
    }
}

/// Copy a complete boxed-array span through the ledger owner. Both wrappers and
/// all signed ranges are authenticated before the owner snapshots the source;
/// the owner then uses its retained-slot barrier for the destination range.
/// No collection or callback occurs while the source snapshot is live.
pub(super) unsafe extern "C" fn prepared_copy_boxed(
    vmctx: *mut crate::context::VMContext,
    descriptor: *const ObjectDescriptor,
    source: *mut u8,
    source_offset: i64,
    destination: *mut u8,
    destination_offset: i64,
    count: i64,
) -> i32 {
    use crate::prepared_control::CallStatus;
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (source, source_len) =
            unsafe { active_boxed_payload(machine, vmctx, source, descriptor) }?;
        let (destination, destination_len) =
            unsafe { active_boxed_payload(machine, vmctx, destination, descriptor) }?;
        let source_offset = checked_element_span(source_offset, source_len)?;
        let destination_offset = checked_element_span(destination_offset, destination_len)?;
        let count = checked_element_span(count, source_len)?;
        machine
            .copy_external_elements(
                source,
                source_offset,
                destination,
                destination_offset,
                count,
            )
            .map_err(boxed_range_error)
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_cas_boxed(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    expected: *mut u8,
    value: *mut u8,
    flag_output: *mut i64,
    value_output: *mut *mut u8,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if flag_output.is_null() || value_output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (published, len) =
            unsafe { active_boxed_payload(machine, vmctx, reference, descriptor) }?;
        let checked_index = usize::try_from(index)
            .ok()
            .filter(|&candidate| candidate < len)
            .ok_or(RuntimeError::ArrayIndexOutOfBounds { index, len })?;
        let old = machine
            .compare_exchange_external_element(published, checked_index, expected, value)
            .map_err(|error| storage_error(error, index))?;
        let (flag, observed) = if old == expected {
            (0, value)
        } else {
            (1, old)
        };
        unsafe {
            flag_output.write(flag);
            value_output.write(observed);
        }
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => array_error(machine, error),
    }
}

pub(super) fn emit_new_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: FuncId,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_new_boxed_value(
        builder,
        pipeline,
        vmctx,
        gc,
        descriptor,
        arguments[0],
        arguments[1],
    )
}

pub(super) fn emit_new_mut_var(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: FuncId,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let length = builder.ins().iconst(types::I64, 1);
    emit_new_boxed_value(
        builder,
        pipeline,
        vmctx,
        gc,
        descriptor,
        length,
        arguments[0],
    )
}

fn emit_new_boxed_value(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: FuncId,
    descriptor: &ObjectDescriptor,
    length: Value,
    initial: Value,
) -> Result<Vec<Value>, super::CompileError> {
    builder.declare_value_needs_stack_map(initial);
    let gc = pipeline.module.declare_func_in_func(gc, builder.func);
    let object = crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, descriptor, gc);
    let header = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    builder.ins().store(MemFlags::trusted(), header, object, 0);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().store(MemFlags::trusted(), zero, object, 8);

    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 4];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function("prepared_new_boxed", Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let call = builder.ins().call(host, &[vmctx, object, length, initial]);
    let status = builder.inst_results(call)[0];
    let success = builder
        .ins()
        .icmp_imm(ir::condcodes::IntCC::Equal, status, 0);
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

pub(super) fn declare_host(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    name: &str,
    parameters: usize,
) -> Result<cranelift_codegen::ir::FuncRef, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); parameters];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    Ok(pipeline.module.declare_func_in_func(host, builder.func))
}

pub(super) fn finish_checked_call(builder: &mut FunctionBuilder<'_>, status: Value) {
    let success = builder
        .ins()
        .icmp_imm(ir::condcodes::IntCC::Equal, status, 0);
    let valid = builder.create_block();
    let invalid = builder.create_block();
    builder.ins().brif(success, valid, &[], invalid, &[]);
    builder.switch_to_block(invalid);
    builder.seal_block(invalid);
    crate::alloc::emit_prepared_failure_return(builder, status);
    builder.switch_to_block(valid);
    builder.seal_block(valid);
}

pub(super) fn output_slot(builder: &mut FunctionBuilder<'_>) -> Value {
    let slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    builder.ins().stack_addr(types::I64, slot, 0)
}

pub(super) fn emit_read_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_read_boxed_value(
        builder,
        pipeline,
        vmctx,
        descriptor,
        arguments[0],
        arguments[1],
    )
}

pub(super) fn emit_read_mut_var(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let index = builder.ins().iconst(types::I64, 0);
    emit_read_boxed_value(builder, pipeline, vmctx, descriptor, arguments[0], index)
}

fn emit_read_boxed_value(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    reference: Value,
    index: Value,
) -> Result<Vec<Value>, super::CompileError> {
    let host = declare_host(builder, pipeline, "prepared_read_boxed", 5)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let output = output_slot(builder);
    let call = builder
        .ins()
        .call(host, &[vmctx, reference, owner, index, output]);
    let status = builder.inst_results(call)[0];
    finish_checked_call(builder, status);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), output, 0);
    builder.declare_value_needs_stack_map(value);
    Ok(vec![value])
}

pub(super) fn emit_write_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_write_boxed_value(
        builder,
        pipeline,
        vmctx,
        descriptor,
        arguments[0],
        arguments[1],
        arguments[2],
    )
}

pub(super) fn emit_write_mut_var(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let index = builder.ins().iconst(types::I64, 0);
    emit_write_boxed_value(
        builder,
        pipeline,
        vmctx,
        descriptor,
        arguments[0],
        index,
        arguments[1],
    )
}

fn emit_write_boxed_value(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    reference: Value,
    index: Value,
    value: Value,
) -> Result<Vec<Value>, super::CompileError> {
    builder.declare_value_needs_stack_map(value);
    let host = declare_host(builder, pipeline, "prepared_write_boxed", 5)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let call = builder
        .ins()
        .call(host, &[vmctx, reference, owner, index, value]);
    let status = builder.inst_results(call)[0];
    finish_checked_call(builder, status);
    Ok(Vec::new())
}

pub(super) fn emit_sizeof_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = declare_host(builder, pipeline, "prepared_sizeof_boxed", 4)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let output = output_slot(builder);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], owner, output]);
    let status = builder.inst_results(call)[0];
    finish_checked_call(builder, status);
    Ok(vec![builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        output,
        0,
    )])
}

pub(super) fn emit_freeze_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = declare_host(builder, pipeline, "prepared_freeze_boxed", 3)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let call = builder.ins().call(host, &[vmctx, arguments[0], owner]);
    let status = builder.inst_results(call)[0];
    finish_checked_call(builder, status);
    builder.declare_value_needs_stack_map(arguments[0]);
    Ok(vec![arguments[0]])
}

pub(super) fn emit_shrink_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = declare_host(builder, pipeline, "prepared_shrink_boxed", 4)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], owner, arguments[1]]);
    let status = builder.inst_results(call)[0];
    finish_checked_call(builder, status);
    Ok(Vec::new())
}

pub(super) fn emit_copy_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    builder.declare_value_needs_stack_map(arguments[0]);
    builder.declare_value_needs_stack_map(arguments[2]);
    let host = declare_host(builder, pipeline, "prepared_copy_boxed", 7)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let call = builder.ins().call(
        host,
        &[
            vmctx,
            owner,
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
            arguments[4],
        ],
    );
    let status = builder.inst_results(call)[0];
    finish_checked_call(builder, status);
    Ok(Vec::new())
}

pub(super) fn emit_cas_boxed(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    builder.declare_value_needs_stack_map(arguments[2]);
    builder.declare_value_needs_stack_map(arguments[3]);
    let host = declare_host(builder, pipeline, "prepared_cas_boxed", 8)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let flag_output = output_slot(builder);
    let value_output = output_slot(builder);
    let call = builder.ins().call(
        host,
        &[
            vmctx,
            arguments[0],
            owner,
            arguments[1],
            arguments[2],
            arguments[3],
            flag_output,
            value_output,
        ],
    );
    let status = builder.inst_results(call)[0];
    finish_checked_call(builder, status);
    let flag = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), flag_output, 0);
    let value = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), value_output, 0);
    builder.declare_value_needs_stack_map(value);
    Ok(vec![flag, value])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::*;

    #[test]
    fn array_operations_require_physical_results_and_exact_state_arguments() {
        let signature = |arguments, results| Signature {
            arguments,
            results: ResultContract::Returns(results),
        };
        let primop = |name: &str| OperationIdentity::PrimOp(name.into());
        assert!(matches!(
            recognize(
                &primop("newSmallArray#"),
                &signature(
                    vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
                    vec![RuntimeRep::UnliftedRef]
                ),
            ),
            Some(ArrayOperation::NewBoxed)
        ));
        assert!(recognize(
            &primop("newSmallArray#"),
            &signature(
                vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
                vec![RuntimeRep::Void, RuntimeRep::UnliftedRef]
            ),
        )
        .is_none());
        for name in ["unsafeFreezeSmallArray#", "unsafeFreezeArray#"] {
            assert!(matches!(
                recognize(
                    &primop(name),
                    &signature(
                        vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
                        vec![RuntimeRep::UnliftedRef]
                    )
                ),
                Some(ArrayOperation::UnsafeFreezeBoxed)
            ));
        }
        assert!(matches!(
            recognize(
                &primop("shrinkSmallMutableArray#"),
                &signature(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Void
                    ],
                    vec![]
                )
            ),
            Some(ArrayOperation::ShrinkSmallBoxed)
        ));
        for name in ["casSmallArray#", "casArray#"] {
            assert!(matches!(
                recognize(
                    &primop(name),
                    &signature(
                        vec![
                            RuntimeRep::UnliftedRef,
                            RuntimeRep::Int(64),
                            RuntimeRep::LiftedRef,
                            RuntimeRep::LiftedRef,
                            RuntimeRep::Void
                        ],
                        vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef]
                    )
                ),
                Some(ArrayOperation::CasBoxed)
            ));
        }
        assert!(matches!(
            recognize(
                &primop("writeArray#"),
                &signature(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::LiftedRef,
                        RuntimeRep::Void
                    ],
                    vec![]
                ),
            ),
            Some(ArrayOperation::WriteBoxed)
        ));
        for name in ["copySmallMutableArray#", "copyMutableArray#"] {
            assert!(matches!(
                recognize(
                    &primop(name),
                    &signature(
                        vec![
                            RuntimeRep::UnliftedRef,
                            RuntimeRep::Int(64),
                            RuntimeRep::UnliftedRef,
                            RuntimeRep::Int(64),
                            RuntimeRep::Int(64),
                            RuntimeRep::Void,
                        ],
                        vec![]
                    )
                ),
                Some(ArrayOperation::CopyBoxed)
            ));
        }
        assert!(recognize(
            &primop("writeArray#"),
            &signature(
                vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::LiftedRef
                ],
                vec![]
            ),
        )
        .is_none());
    }

    #[test]
    fn mut_var_operations_require_exact_state_threaded_signatures() {
        let signature = |arguments, results| Signature {
            arguments,
            results: ResultContract::Returns(results),
        };
        let primop = |name: &str| OperationIdentity::PrimOp(name.into());
        assert!(matches!(
            recognize(
                &primop("newMutVar#"),
                &signature(
                    vec![RuntimeRep::LiftedRef, RuntimeRep::Void],
                    vec![RuntimeRep::UnliftedRef],
                ),
            ),
            Some(ArrayOperation::NewMutVar)
        ));
        assert!(matches!(
            recognize(
                &primop("readMutVar#"),
                &signature(
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
                    vec![RuntimeRep::LiftedRef],
                ),
            ),
            Some(ArrayOperation::ReadMutVar)
        ));
        assert!(matches!(
            recognize(
                &primop("writeMutVar#"),
                &signature(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::LiftedRef,
                        RuntimeRep::Void,
                    ],
                    vec![],
                ),
            ),
            Some(ArrayOperation::WriteMutVar)
        ));
        assert!(recognize(
            &primop("writeMutVar#"),
            &signature(vec![RuntimeRep::UnliftedRef, RuntimeRep::LiftedRef], vec![],),
        )
        .is_none());
    }

    fn empty_constructor(index: u64) -> ConstructorDecl {
        ConstructorDecl {
            identity: testing::identity("Arrays", &format!("C{index}")),
            family: testing::identity("Arrays", &format!("T{index}")),
            host_id: tidepool_repr::DataConId(1000 + index),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        }
    }

    fn flow_program(
        names: [&str; 3],
        read_index: i64,
        garbage: usize,
    ) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        let mut_var = names[0] == "newMutVar#";
        wire.signatures.extend([
            Signature {
                arguments: if mut_var {
                    vec![RuntimeRep::LiftedRef, RuntimeRep::Void]
                } else {
                    vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void]
                },
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: if mut_var {
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::LiftedRef,
                        RuntimeRep::Void,
                    ]
                } else {
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::LiftedRef,
                        RuntimeRep::Void,
                    ]
                },
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: if mut_var {
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Void]
                } else if names[2].starts_with("index") {
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)]
                } else {
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Void,
                    ]
                },
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
        ]);
        wire.constructors = vec![empty_constructor(0), empty_constructor(1)];
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Arrays", "initial"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }));
        wire.operations = names
            .iter()
            .enumerate()
            .map(|(index, name)| OperationDecl {
                identity: OperationIdentity::PrimOp((*name).into()),
                signature: SignatureId(index as u32 + 1),
            })
            .collect();
        let int = |value: i64| {
            Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: value.to_be_bytes().to_vec(),
            })
        };
        let mut nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: if mut_var {
                    vec![Atom::Ref(ValueRef::Local(ValueId(1))), Atom::Void]
                } else {
                    vec![int(1), Atom::Ref(ValueRef::Local(ValueId(1))), Atom::Void]
                },
            },
            ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: if mut_var {
                    vec![
                        Atom::Ref(ValueRef::Local(ValueId(100))),
                        Atom::Ref(ValueRef::Local(ValueId(101))),
                        Atom::Void,
                    ]
                } else {
                    vec![
                        Atom::Ref(ValueRef::Local(ValueId(100))),
                        int(0),
                        Atom::Ref(ValueRef::Local(ValueId(101))),
                        Atom::Void,
                    ]
                },
            },
            ExprFrame::Operation {
                operation: OperationId(2),
                arguments: if mut_var {
                    vec![Atom::Ref(ValueRef::Local(ValueId(100))), Atom::Void]
                } else if names[2].starts_with("index") {
                    vec![Atom::Ref(ValueRef::Local(ValueId(100))), int(read_index)]
                } else {
                    vec![
                        Atom::Ref(ValueRef::Local(ValueId(100))),
                        int(read_index),
                        Atom::Void,
                    ]
                },
            },
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(102)))]),
            ExprFrame::Case {
                scrutinee: 3,
                binder: ValueId(102),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 4,
                }],
            },
        ];
        let mut after_write = 5;
        for index in 0..garbage {
            let construct = nodes.len();
            nodes.push(ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![],
            });
            let case = nodes.len();
            nodes.push(ExprFrame::Case {
                scrutinee: construct,
                binder: ValueId(200 + index as u32),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: after_write,
                }],
            });
            after_write = case;
        }
        let write_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 2,
            binder: ValueId(103),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: after_write,
            }],
        });
        let replacement_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 1,
            binder: ValueId(101),
            kind: CaseKind::Polymorphic,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: write_case,
            }],
        });
        let new_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(104),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![ValueId(100)],
                body: replacement_case,
            }],
        });
        wire.expressions.nodes = nodes;
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                *body = new_case;
            }
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        crate::prepared_program::CompiledProgram::compile(
            &linked,
            crate::prepared_program::TopSlotBase::ZERO,
        )
        .unwrap()
    }

    fn copy_program(
        copy_name: &str,
        source_offset: i64,
        destination_offset: i64,
        count: i64,
        read_index: i64,
        garbage: usize,
        distinct_destination: bool,
    ) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.extend([
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::LiftedRef,
                    RuntimeRep::Void,
                ],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Int(64),
                    RuntimeRep::Void,
                ],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Void,
                ],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
        ]);
        wire.constructors = (0_u64..=5).map(empty_constructor).collect();
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Arrays", "initial"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }));
        wire.operations = [
            "newSmallArray#",
            "writeSmallArray#",
            copy_name,
            "readSmallArray#",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(index as u32 + 1),
        })
        .collect();
        let int = |value: i64| {
            Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: value.to_be_bytes().to_vec(),
            })
        };
        let local = |id| Atom::Ref(ValueRef::Local(ValueId(id)));
        let mut nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![int(5), local(1), Atom::Void],
            },
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![int(5), local(1), Atom::Void],
            },
        ];
        let mut writes = Vec::new();
        for index in 0_u32..5 {
            let value = nodes.len();
            nodes.push(ExprFrame::Construct {
                constructor: ConstructorId(index + 1),
                fields: vec![],
            });
            let write = nodes.len();
            nodes.push(ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![
                    local(100),
                    int(i64::from(index)),
                    local(200 + index),
                    Atom::Void,
                ],
            });
            writes.push((write, value, index));
        }
        let copy = nodes.len();
        nodes.push(ExprFrame::Operation {
            operation: OperationId(2),
            arguments: vec![
                local(100),
                int(source_offset),
                if distinct_destination {
                    local(101)
                } else {
                    local(100)
                },
                int(destination_offset),
                int(count),
                Atom::Void,
            ],
        });
        let read = nodes.len();
        nodes.push(ExprFrame::Operation {
            operation: OperationId(3),
            arguments: vec![
                if distinct_destination {
                    local(101)
                } else {
                    local(100)
                },
                int(read_index),
                Atom::Void,
            ],
        });
        let ret = nodes.len();
        nodes.push(ExprFrame::Return(vec![local(102)]));
        let mut continuation = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: read,
            binder: ValueId(102),
            kind: CaseKind::Polymorphic,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: ret,
            }],
        });
        for index in 0..garbage {
            let construct = nodes.len();
            nodes.push(ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![],
            });
            let case = nodes.len();
            nodes.push(ExprFrame::Case {
                scrutinee: construct,
                binder: ValueId(300 + index as u32),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: continuation,
                }],
            });
            continuation = case;
        }
        let copy_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: copy,
            binder: ValueId(103),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: continuation,
            }],
        });
        continuation = copy_case;
        for (write, construct, index) in writes.into_iter().rev() {
            let write_case = nodes.len();
            nodes.push(ExprFrame::Case {
                scrutinee: write,
                binder: ValueId(400 + write_case as u32),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: continuation,
                }],
            });
            let value_case = nodes.len();
            nodes.push(ExprFrame::Case {
                scrutinee: construct,
                binder: ValueId(200 + index),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: write_case,
                }],
            });
            continuation = value_case;
        }
        let destination_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 1,
            binder: ValueId(105),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![ValueId(101)],
                body: continuation,
            }],
        });
        let root = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(104),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![ValueId(100)],
                body: destination_case,
            }],
        });
        wire.expressions.nodes = nodes;
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                *body = root;
            }
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        crate::prepared_program::CompiledProgram::compile(
            &linked,
            crate::prepared_program::TopSlotBase::ZERO,
        )
        .unwrap()
    }

    fn size_program(new_name: &str, size_name: &str) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Int(64)]);
        wire.signatures.extend([
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![RuntimeRep::UnliftedRef],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
            },
        ]);
        wire.constructors = vec![empty_constructor(0)];
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Arrays", "initial"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }));
        wire.operations = [new_name, size_name]
            .into_iter()
            .enumerate()
            .map(|(index, name)| OperationDecl {
                identity: OperationIdentity::PrimOp(name.into()),
                signature: SignatureId(index as u32 + 1),
            })
            .collect();
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![
                    Atom::Scalar(ScalarLiteral::Int {
                        bits: 64,
                        bytes: 3_i64.to_be_bytes().to_vec(),
                    }),
                    Atom::Ref(ValueRef::Local(ValueId(1))),
                    Atom::Void,
                ],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(100)))],
            },
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(101)))]),
            ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(101),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 2,
                }],
            },
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(102),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(100)],
                    body: 3,
                }],
            },
        ];
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                *body = 4;
            }
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        crate::prepared_program::CompiledProgram::compile(
            &linked,
            crate::prepared_program::TopSlotBase::ZERO,
        )
        .unwrap()
    }

    fn frozen_shrink_program(index_tail: bool) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        let final_rep = if index_tail {
            RuntimeRep::LiftedRef
        } else {
            RuntimeRep::Int(64)
        };
        wire.signatures[0].results = ResultContract::Returns(vec![final_rep.clone()]);
        wire.signatures.extend([
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Void,
                ],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: if index_tail {
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)]
                } else {
                    vec![RuntimeRep::UnliftedRef]
                },
                results: ResultContract::Returns(vec![final_rep.clone()]),
            },
        ]);
        wire.constructors = vec![empty_constructor(0)];
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Arrays", "initial"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }));
        wire.operations = [
            "newSmallArray#",
            "unsafeFreezeSmallArray#",
            "shrinkSmallMutableArray#",
            if index_tail {
                "indexSmallArray#"
            } else {
                "sizeofSmallArray#"
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(index as u32 + 1),
        })
        .collect();
        let int = |value: i64| {
            Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: value.to_be_bytes().to_vec(),
            })
        };
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![int(3), Atom::Ref(ValueRef::Local(ValueId(1))), Atom::Void],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(100))), Atom::Void],
            },
            ExprFrame::Operation {
                operation: OperationId(2),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(100))), int(1), Atom::Void],
            },
            ExprFrame::Operation {
                operation: OperationId(3),
                arguments: if index_tail {
                    vec![Atom::Ref(ValueRef::Local(ValueId(101))), int(2)]
                } else {
                    vec![Atom::Ref(ValueRef::Local(ValueId(101)))]
                },
            },
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(102)))]),
            ExprFrame::Case {
                scrutinee: 3,
                binder: ValueId(102),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![final_rep]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 4,
                }],
            },
            ExprFrame::Case {
                scrutinee: 2,
                binder: ValueId(103),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 5,
                }],
            },
            ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(104),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(101)],
                    body: 6,
                }],
            },
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(105),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(100)],
                    body: 7,
                }],
            },
        ];
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                *body = 8;
            }
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        crate::prepared_program::CompiledProgram::compile(
            &linked,
            crate::prepared_program::TopSlotBase::ZERO,
        )
        .unwrap()
    }

    fn cas_program(name: &str, success: bool) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results =
            ResultContract::Returns(vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef]);
        wire.signatures.extend([
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::LiftedRef,
                    RuntimeRep::LiftedRef,
                    RuntimeRep::Void,
                ],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef]),
            },
        ]);
        wire.constructors = vec![empty_constructor(0), empty_constructor(1)];
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Arrays", "initial"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }));
        wire.operations = ["newSmallArray#", name]
            .into_iter()
            .enumerate()
            .map(|(index, name)| OperationDecl {
                identity: OperationIdentity::PrimOp(name.into()),
                signature: SignatureId(index as u32 + 1),
            })
            .collect();
        let initial = Atom::Ref(ValueRef::Local(ValueId(1)));
        let replacement = Atom::Ref(ValueRef::Local(ValueId(101)));
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![
                    Atom::Scalar(ScalarLiteral::Int {
                        bits: 64,
                        bytes: 1_i64.to_be_bytes().to_vec(),
                    }),
                    initial.clone(),
                    Atom::Void,
                ],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![
                    Atom::Ref(ValueRef::Local(ValueId(100))),
                    Atom::Scalar(ScalarLiteral::Int {
                        bits: 64,
                        bytes: 0_i64.to_be_bytes().to_vec(),
                    }),
                    if success {
                        initial
                    } else {
                        replacement.clone()
                    },
                    replacement,
                    Atom::Void,
                ],
            },
            ExprFrame::Return(vec![
                Atom::Ref(ValueRef::Local(ValueId(102))),
                Atom::Ref(ValueRef::Local(ValueId(103))),
            ]),
            ExprFrame::Case {
                scrutinee: 2,
                binder: ValueId(104),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![
                    RuntimeRep::Int(64),
                    RuntimeRep::LiftedRef,
                ]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(102), ValueId(103)],
                    body: 3,
                }],
            },
            ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(101),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 4,
                }],
            },
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(105),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(100)],
                    body: 5,
                }],
            },
        ];
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                *body = 6;
            }
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        crate::prepared_program::CompiledProgram::compile(
            &linked,
            crate::prepared_program::TopSlotBase::ZERO,
        )
        .unwrap()
    }

    #[test]
    fn boxed_arrays_and_mut_vars_remain_unobservable_after_result_collection() {
        for (name, signature, arguments) in [
            (
                "newSmallArray#",
                vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
                vec![
                    Atom::Scalar(ScalarLiteral::Int {
                        bits: 64,
                        bytes: 3_i64.to_be_bytes().to_vec(),
                    }),
                    Atom::Ref(ValueRef::Local(ValueId(1))),
                    Atom::Void,
                ],
            ),
            (
                "newMutVar#",
                vec![RuntimeRep::LiftedRef, RuntimeRep::Void],
                vec![Atom::Ref(ValueRef::Local(ValueId(1))), Atom::Void],
            ),
        ] {
            let mut wire = testing::wire_program();
            wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::UnliftedRef]);
            wire.signatures.push(Signature {
                arguments: signature,
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            });
            wire.constructors.push(ConstructorDecl {
                identity: testing::identity("Arrays", "C"),
                family: testing::identity("Arrays", "T"),
                host_id: tidepool_repr::DataConId(999),
                result_rep: RuntimeRep::LiftedRef,
                tag: 1,
                family_size: 1,
                field_reps: vec![],
                strict_fields: vec![],
                layout: CheckedLayout {
                    fields: vec![],
                    alignment: 1,
                    payload_size: 0,
                    root_mask: vec![],
                },
            });
            wire.bindings.push(Group::NonRecursive(TopBinding {
                identity: testing::identity("Arrays", "initial"),
                binding: HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(0),
                        fields: vec![],
                    },
                },
            }));
            wire.operations.push(OperationDecl {
                identity: OperationIdentity::PrimOp(name.into()),
                signature: SignatureId(1),
            });
            wire.expressions.nodes = vec![
                ExprFrame::Operation {
                    operation: OperationId(0),
                    arguments,
                },
                ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(4)))]),
                ExprFrame::Case {
                    scrutinee: 0,
                    binder: ValueId(2),
                    kind: CaseKind::MultiValue,
                    scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                    alternatives: vec![Alternative {
                        pattern: AlternativePattern::Default,
                        binders: vec![ValueId(4)],
                        body: 1,
                    }],
                },
            ];
            if let Group::NonRecursive(top) = &mut wire.bindings[0] {
                if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                    *body = 2;
                }
            }
            let linked =
                link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
            let program = crate::prepared_program::CompiledProgram::compile(
                &linked,
                crate::prepared_program::TopSlotBase::ZERO,
            )
            .unwrap();
            let result = program.run_entry(
                ValueId(0),
                &[],
                &crate::prepared_program::RunOptions {
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            );
            assert!(matches!(
                result,
                Err(crate::prepared_program::ExecutionError::Observation(
                    crate::prepared_program::ObservationFailure::Unobservable(
                        tidepool_heap::execution_descriptor::ObjectKind::External(
                            ExternalStorageKind::BoxedArray
                        )
                    )
                ))
            ));
        }
    }

    #[test]
    fn prepared_small_array_write_read_preserves_dynamic_child_through_gc() {
        let program = flow_program(
            ["newSmallArray#", "writeSmallArray#", "readSmallArray#"],
            0,
            24,
        );
        let result = program
            .run_entry(
                ValueId(0),
                &[],
                &crate::prepared_program::RunOptions {
                    nursery_bytes: 128,
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(
            result.collections >= 2,
            "fixture must move the array and child"
        );
        assert!(matches!(result.values.as_slice(),
            [tidepool_bridge::Value::Con(id, fields)] if *id == tidepool_repr::DataConId(1001) && fields.is_empty()));
    }

    #[test]
    fn prepared_mut_var_write_read_preserves_dynamic_child_through_gc() {
        let program = flow_program(["newMutVar#", "writeMutVar#", "readMutVar#"], 0, 24);
        let result = program
            .run_entry(
                ValueId(0),
                &[],
                &crate::prepared_program::RunOptions {
                    nursery_bytes: 128,
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(
            result.collections >= 2,
            "fixture must move the mutable cell and child"
        );
        assert!(matches!(result.values.as_slice(),
            [tidepool_bridge::Value::Con(id, fields)] if *id == tidepool_repr::DataConId(1001) && fields.is_empty()));
    }

    #[test]
    fn prepared_mutable_array_copy_handles_overlap_distinct_ranges_and_gc() {
        for name in ["copySmallMutableArray#", "copyMutableArray#"] {
            for (source_offset, destination_offset, read_index, expected) in [
                (0, 1, 1, 1001), // shift right: source[0] reaches destination[1]
                (1, 0, 0, 1002), // shift left: source[1] reaches destination[0]
            ] {
                let result = copy_program(
                    name,
                    source_offset,
                    destination_offset,
                    4,
                    read_index,
                    32,
                    false,
                )
                .run_entry(
                    ValueId(0),
                    &[],
                    &crate::prepared_program::RunOptions {
                        nursery_bytes: 128,
                        collect_before_observation: true,
                        ..Default::default()
                    },
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
                assert!(result.collections >= 2);
                assert!(matches!(
                    result.values.as_slice(),
                    [tidepool_bridge::Value::Con(id, fields)]
                        if *id == tidepool_repr::DataConId(expected) && fields.is_empty()
                ));
            }
            let result = copy_program(name, 1, 2, 2, 2, 32, true)
                .run_entry(
                    ValueId(0),
                    &[],
                    &crate::prepared_program::RunOptions {
                        nursery_bytes: 128,
                        collect_before_observation: true,
                        ..Default::default()
                    },
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
            assert!(matches!(
                result.values.as_slice(),
                [tidepool_bridge::Value::Con(id, fields)]
                    if *id == tidepool_repr::DataConId(1002) && fields.is_empty()
            ));
        }
    }

    #[test]
    fn prepared_mutable_array_copy_bounds_fail_before_any_write() {
        for (source_offset, destination_offset, count) in [(-1, 0, 1), (0, -1, 1), (0, 0, 6)] {
            let error = copy_program(
                "copySmallMutableArray#",
                source_offset,
                destination_offset,
                count,
                0,
                0,
                false,
            )
            .run_entry(
                ValueId(0),
                &[],
                &Default::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                crate::prepared_program::ExecutionError::Runtime(failure)
                    if failure.disposition == crate::machine_state::MachineDisposition::Reusable
                        && matches!(failure.cause, crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { .. })
            ));
        }
    }

    #[test]
    fn prepared_mutable_array_copy_host_rejects_destination_overrun_without_write() {
        use tidepool_repr::execution_schema::{Architecture, Endianness, TargetDescriptor};
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let descriptor =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let machine = crate::machine_state::MachineState::new();
        let extent = descriptor.allocation_extent() as usize;
        machine
            .install_prepared_buffer(
                vec![0_u64; extent / 8 * 2],
                vec![descriptor.clone(), descriptor.clone()],
            )
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let second = unsafe { start.add(extent) };
        let source = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        let destination = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        machine
            .store_external_elements(
                source,
                0,
                &[1usize as *mut u8, 2usize as *mut u8, 3usize as *mut u8],
            )
            .unwrap();
        machine
            .store_external_elements(
                destination,
                0,
                &[9usize as *mut u8, 9usize as *mut u8, 9usize as *mut u8],
            )
            .unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor.initialize_header(second);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(source);
            descriptor
                .external_payload_slot(second, extent)
                .unwrap()
                .write(destination);
        }
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        vmctx.alloc_ptr = unsafe { second.add(extent) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        let status = unsafe {
            prepared_copy_boxed(
                &mut vmctx,
                Arc::as_ptr(&descriptor),
                (start as usize | usize::from(descriptor.tag())) as *mut u8,
                0,
                (second as usize | usize::from(descriptor.tag())) as *mut u8,
                2,
                2,
            )
        };
        assert_eq!(
            status,
            crate::prepared_control::CallStatus::LanguageFailure as i32
        );
        assert_eq!(
            machine.take_runtime_error(),
            Some(crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: 3, len: 3 })
        );
        let slots = unsafe { std::slice::from_raw_parts(destination.add(8).cast::<*mut u8>(), 3) };
        assert_eq!(
            slots,
            [9usize as *mut u8, 9usize as *mut u8, 9usize as *mut u8]
        );
    }

    #[test]
    fn prepared_array_index_has_typed_bounds_failure() {
        let program = flow_program(["newArray#", "writeArray#", "indexArray#"], -1, 0);
        let error = program
            .run_entry(
                ValueId(0),
                &[],
                &Default::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap_err();
        assert!(matches!(error,
            crate::prepared_program::ExecutionError::Runtime(failure)
                if failure.cause == crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: -1, len: 1 }
                    && failure.disposition == crate::machine_state::MachineDisposition::Reusable));
    }

    #[test]
    fn prepared_array_index_returns_replacement_and_size_is_scalar() {
        let program = flow_program(["newArray#", "writeArray#", "indexArray#"], 0, 0);
        let result = program
            .run_entry(
                ValueId(0),
                &[],
                &Default::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(matches!(result.values.as_slice(),
            [tidepool_bridge::Value::Con(id, fields)] if *id == tidepool_repr::DataConId(1001) && fields.is_empty()));
        for (new_name, size_name) in [
            ("newSmallArray#", "sizeofSmallArray#"),
            ("newArray#", "sizeofArray#"),
        ] {
            let program = size_program(new_name, size_name);
            let result = program
                .run_entry(
                    ValueId(0),
                    &[],
                    &Default::default(),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
            assert!(matches!(
                result.values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(
                    3
                ))]
            ));
        }
    }

    #[test]
    fn prepared_freeze_alias_observes_shrink_and_rejects_old_tail() {
        let result = frozen_shrink_program(false)
            .run_entry(
                ValueId(0),
                &[],
                &Default::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(
                1
            ))]
        ));
        let error = frozen_shrink_program(true)
            .run_entry(
                ValueId(0),
                &[],
                &Default::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap_err();
        assert!(matches!(error,
            crate::prepared_program::ExecutionError::Runtime(failure)
                if failure.cause == crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: 2, len: 1 }
                    && failure.disposition == crate::machine_state::MachineDisposition::Reusable));
    }

    #[test]
    fn prepared_cas_returns_flag_then_post_operation_value() {
        for (name, success) in [("casSmallArray#", true), ("casArray#", false)] {
            let result = cas_program(name, success)
                .run_entry(
                    ValueId(0),
                    &[],
                    &crate::prepared_program::RunOptions {
                        collect_before_observation: true,
                        ..Default::default()
                    },
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
            let expected_flag = i64::from(!success);
            let expected_constructor = tidepool_repr::DataConId(if success { 1001 } else { 1000 });
            assert!(matches!(result.values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(flag)),
                 tidepool_bridge::Value::Con(id, fields)]
                    if *flag == expected_flag && *id == expected_constructor && fields.is_empty()));
        }
    }

    #[test]
    fn revoked_array_payload_rejects_host_reads_without_publishing_output() {
        use tidepool_repr::execution_schema::{Architecture, Endianness, TargetDescriptor};
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let descriptor =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let machine = crate::machine_state::MachineState::new();
        let extent = descriptor.allocation_extent() as usize;
        machine
            .install_prepared_buffer(vec![0_u64; extent / 8], vec![descriptor.clone()])
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 1)
            .unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(payload);
        }
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        vmctx.alloc_ptr = unsafe { start.add(extent) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        machine
            .revoke_external_payload(payload, ExternalStorageKind::BoxedArray)
            .unwrap();
        let mut output = 13usize as *mut u8;
        let status = unsafe {
            prepared_read_boxed(
                &mut vmctx,
                (start as usize | usize::from(descriptor.tag())) as *mut u8,
                Arc::as_ptr(&descriptor),
                0,
                &mut output,
            )
        };
        assert_eq!(
            status,
            crate::prepared_control::CallStatus::IntegrityFailure as i32
        );
        assert_eq!(output, 13usize as *mut u8);
        assert_eq!(
            machine.take_runtime_error(),
            Some(crate::host_fns::RuntimeError::BadPointer)
        );
        assert_eq!(machine.external_storage_stats().live_objects, 1);
    }

    #[test]
    fn boxed_array_host_rejects_mut_var_descriptor_identity() {
        use tidepool_repr::execution_schema::{Architecture, Endianness, TargetDescriptor};
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let boxed_array =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let mut_var =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        assert_ne!(
            boxed_array.initial_header_word(),
            mut_var.initial_header_word()
        );
        let machine = crate::machine_state::MachineState::new();
        let extent = mut_var.allocation_extent() as usize;
        machine
            .install_prepared_buffer(
                vec![0_u64; extent / 8],
                vec![boxed_array.clone(), mut_var.clone()],
            )
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 1)
            .unwrap();
        unsafe {
            mut_var.initialize_header(start);
            mut_var
                .external_payload_slot(start, extent)
                .unwrap()
                .write(payload);
        }
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        vmctx.alloc_ptr = unsafe { start.add(extent) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        let reference = (start as usize | usize::from(mut_var.tag())) as *mut u8;
        let sentinel = 13usize as *mut u8;
        let mut output = sentinel;
        let status = unsafe {
            prepared_read_boxed(
                &mut vmctx,
                reference,
                Arc::as_ptr(&boxed_array),
                0,
                &mut output,
            )
        };
        assert_eq!(
            status,
            crate::prepared_control::CallStatus::IntegrityFailure as i32
        );
        assert_eq!(output, sentinel);
        assert_eq!(
            machine.take_runtime_error(),
            Some(crate::host_fns::RuntimeError::BadPointer)
        );
    }

    #[test]
    fn boxed_freeze_shrink_and_cas_keep_alias_identity_and_exact_results() {
        use tidepool_repr::execution_schema::{Architecture, Endianness, TargetDescriptor};
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let descriptor =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let machine = crate::machine_state::MachineState::new();
        let extent = descriptor.allocation_extent() as usize;
        machine
            .install_prepared_buffer(vec![0_u64; extent / 8], vec![descriptor.clone()])
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(payload);
        }
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        vmctx.alloc_ptr = unsafe { start.add(extent) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        let alias = (start as usize | usize::from(descriptor.tag())) as *mut u8;
        let original = 0x10usize as *mut u8;
        let replacement = 0x20usize as *mut u8;
        let absent = 0x30usize as *mut u8;
        machine
            .store_external_element(payload, 0, original)
            .unwrap();
        machine
            .retain_external_payloads(&[(payload as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();

        assert_eq!(
            unsafe { prepared_freeze_boxed(&mut vmctx, alias, Arc::as_ptr(&descriptor)) },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!(
            unsafe {
                descriptor
                    .external_payload_slot(start, extent)
                    .unwrap()
                    .read()
            },
            payload
        );

        let mut flag = -1;
        let mut observed = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                prepared_cas_boxed(
                    &mut vmctx,
                    alias,
                    Arc::as_ptr(&descriptor),
                    0,
                    original,
                    replacement,
                    &mut flag,
                    &mut observed,
                )
            },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!((flag, observed), (0, replacement));
        assert_eq!(
            unsafe {
                prepared_cas_boxed(
                    &mut vmctx,
                    alias,
                    Arc::as_ptr(&descriptor),
                    0,
                    absent,
                    original,
                    &mut flag,
                    &mut observed,
                )
            },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!((flag, observed), (1, replacement));

        assert_eq!(
            unsafe { prepared_shrink_boxed(&mut vmctx, alias, Arc::as_ptr(&descriptor), 1) },
            crate::prepared_control::CallStatus::Success as i32
        );
        let mut length = -1;
        assert_eq!(
            unsafe {
                prepared_sizeof_boxed(&mut vmctx, alias, Arc::as_ptr(&descriptor), &mut length)
            },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!(length, 1);
        assert_eq!(
            machine
                .external_active_view(payload, ExternalStorageKind::BoxedArray)
                .unwrap()
                .logical_len,
            1
        );
        let mut tail = absent;
        assert_eq!(
            unsafe {
                prepared_read_boxed(&mut vmctx, alias, Arc::as_ptr(&descriptor), 1, &mut tail)
            },
            crate::prepared_control::CallStatus::LanguageFailure as i32
        );
        assert_eq!(tail, absent);
        assert_eq!(
            machine.take_runtime_error(),
            Some(crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: 1, len: 1 })
        );
    }
}
