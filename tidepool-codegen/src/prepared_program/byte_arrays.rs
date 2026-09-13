//! Descriptor-backed byte arrays. Host calls do not collect; wrappers are
//! reserved and initialized before allocating external bytes.

use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
use cranelift_codegen::ir::{types, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;
use tidepool_heap::{
    execution_descriptor::ObjectDescriptor,
    external_storage::{ExternalStorageKind, ExternalStorageValidationError},
};
use tidepool_repr::execution_schema::{OperationIdentity, ResultContract, RuntimeRep, Signature};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Element {
    Address,
    Word8,
    Word64,
    Int64,
}
impl Element {
    fn bytes(self) -> usize {
        match self {
            Self::Address => 8,
            Self::Word8 => 1,
            Self::Word64 => 8,
            Self::Int64 => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ByteOperation {
    New,
    Contents,
    Resize,
    Freeze,
    Size,
    Shrink,
    Copy,
    Compare,
    Read(Element),
    Write(Element),
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<ByteOperation> {
    use RuntimeRep::*;
    let OperationIdentity::PrimOp(name) = identity else {
        return None;
    };
    match name.as_str() {
        "newByteArray#"
            if signature.arguments == [Int(64), Void]
                && signature.results == ResultContract::Returns(vec![UnliftedRef]) =>
        {
            Some(ByteOperation::New)
        }
        "newPinnedByteArray#"
            if signature.arguments == [Int(64), Void]
                && signature.results == ResultContract::Returns(vec![UnliftedRef]) =>
        {
            Some(ByteOperation::New)
        }
        "byteArrayContents#" | "mutableByteArrayContents#"
            if signature.arguments == [UnliftedRef]
                && signature.results == ResultContract::Returns(vec![Address]) =>
        {
            Some(ByteOperation::Contents)
        }
        "resizeMutableByteArray#"
            if signature.arguments == [UnliftedRef, Int(64), Void]
                && signature.results == ResultContract::Returns(vec![UnliftedRef]) =>
        {
            Some(ByteOperation::Resize)
        }
        "unsafeFreezeByteArray#"
            if signature.arguments == [UnliftedRef, Void]
                && signature.results == ResultContract::Returns(vec![UnliftedRef]) =>
        {
            Some(ByteOperation::Freeze)
        }
        "sizeofByteArray#"
            if signature.arguments == [UnliftedRef]
                && signature.results == ResultContract::Returns(vec![Int(64)]) =>
        {
            Some(ByteOperation::Size)
        }
        "getSizeofMutableByteArray#"
            if signature.arguments == [UnliftedRef, Void]
                && signature.results == ResultContract::Returns(vec![Int(64)]) =>
        {
            Some(ByteOperation::Size)
        }
        "shrinkMutableByteArray#"
            if signature.arguments == [UnliftedRef, Int(64), Void]
                && signature.results == ResultContract::Returns(vec![]) =>
        {
            Some(ByteOperation::Shrink)
        }
        "copyByteArray#"
            if signature.arguments
                == [UnliftedRef, Int(64), UnliftedRef, Int(64), Int(64), Void]
                && signature.results == ResultContract::Returns(vec![]) =>
        {
            Some(ByteOperation::Copy)
        }
        "compareByteArrays#"
            if signature.arguments == [UnliftedRef, Int(64), UnliftedRef, Int(64), Int(64)]
                && signature.results == ResultContract::Returns(vec![Int(64)]) =>
        {
            Some(ByteOperation::Compare)
        }
        "readWord8Array#"
            if signature.arguments == [UnliftedRef, Int(64), Void]
                && signature.results == ResultContract::Returns(vec![Word(8)]) =>
        {
            Some(ByteOperation::Read(Element::Word8))
        }
        "indexWord8Array#"
            if signature.arguments == [UnliftedRef, Int(64)]
                && signature.results == ResultContract::Returns(vec![Word(8)]) =>
        {
            Some(ByteOperation::Read(Element::Word8))
        }
        "writeWord8Array#"
            if signature.arguments == [UnliftedRef, Int(64), Word(8), Void]
                && signature.results == ResultContract::Returns(vec![]) =>
        {
            Some(ByteOperation::Write(Element::Word8))
        }
        "readWordArray#"
            if signature.arguments == [UnliftedRef, Int(64), Void]
                && signature.results == ResultContract::Returns(vec![Word(64)]) =>
        {
            Some(ByteOperation::Read(Element::Word64))
        }
        "indexWordArray#"
            if signature.arguments == [UnliftedRef, Int(64)]
                && signature.results == ResultContract::Returns(vec![Word(64)]) =>
        {
            Some(ByteOperation::Read(Element::Word64))
        }
        "indexAddrArray#"
            if signature.arguments == [UnliftedRef, Int(64)]
                && signature.results == ResultContract::Returns(vec![Address]) =>
        {
            Some(ByteOperation::Read(Element::Address))
        }
        "writeWordArray#"
            if signature.arguments == [UnliftedRef, Int(64), Word(64), Void]
                && signature.results == ResultContract::Returns(vec![]) =>
        {
            Some(ByteOperation::Write(Element::Word64))
        }
        "readIntArray#"
            if signature.arguments == [UnliftedRef, Int(64), Void]
                && signature.results == ResultContract::Returns(vec![Int(64)]) =>
        {
            Some(ByteOperation::Read(Element::Int64))
        }
        "indexIntArray#"
            if signature.arguments == [UnliftedRef, Int(64)]
                && signature.results == ResultContract::Returns(vec![Int(64)]) =>
        {
            Some(ByteOperation::Read(Element::Int64))
        }
        "writeIntArray#"
            if signature.arguments == [UnliftedRef, Int(64), Int(64), Void]
                && signature.results == ResultContract::Returns(vec![]) =>
        {
            Some(ByteOperation::Write(Element::Int64))
        }
        _ => None,
    }
}

/// Proves the whole indexed element fits; diagnostics count elements, not bytes.
fn checked_offset(index: i64, byte_len: usize, element: Element) -> Result<usize, RuntimeError> {
    let width = element.bytes();
    let len = byte_len / width;
    let index = usize::try_from(index)
        .ok()
        .filter(|i| *i < len)
        .ok_or(RuntimeError::ArrayIndexOutOfBounds { index, len })?;
    Ok(index * width)
}

unsafe fn active_bytes(
    machine: &crate::machine_state::MachineState,
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
) -> Result<(*mut u8, usize), RuntimeError> {
    unsafe {
        super::arrays::active_payload(
            machine,
            vmctx,
            reference,
            descriptor,
            ExternalStorageKind::Bytes,
        )
    }
}

/// # Safety
/// Generated code has reserved the wrapper and initialized its descriptor
/// header and handle slot. No collection or re-entry occurs in this call.
pub(super) unsafe extern "C" fn prepared_new_bytes(
    vmctx: *mut crate::context::VMContext,
    wrapper: *mut u8,
    length: i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let payload = usize::try_from(length).ok().and_then(|length| {
        machine
            .allocate_external_storage(ExternalStorageKind::Bytes, length)
            .ok()
    });
    let Some(payload) = payload else {
        return super::arrays::array_error(machine, RuntimeError::HeapOverflow);
    };
    unsafe { wrapper.add(8).cast::<*mut u8>().write(payload) };
    CallStatus::Success as i32
}

/// This host call cannot collect. After authenticating the old handle, the
/// owner allocates and copies before revoking it; publishing cannot fail.
///
/// # Safety
/// `vmctx` and `descriptor` come from the prepared program. `wrapper` is its
/// newly reserved object with a valid descriptor header and empty handle slot.
pub(super) unsafe extern "C" fn prepared_resize_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    wrapper: *mut u8,
    new_len: i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (published, old_len) = unsafe { active_bytes(machine, vmctx, reference, descriptor) }?;
        let new_len =
            usize::try_from(new_len).map_err(|_| RuntimeError::ArrayIndexOutOfBounds {
                index: new_len,
                len: old_len,
            })?;
        let replacement = machine
            .resize_external_bytes(published, new_len)
            .map_err(|error| match error {
                ExternalStorageValidationError::SpanOverflow { .. }
                | ExternalStorageValidationError::BookkeepingAllocation => {
                    RuntimeError::HeapOverflow
                }
                other => super::arrays::storage_error(other, new_len as i64),
            })?;
        unsafe { wrapper.add(8).cast::<*mut u8>().write(replacement) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_freeze_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    match unsafe { active_bytes(machine, vmctx, reference, descriptor) } {
        Ok(_) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

/// Publish the data address only after authenticating both the managed wrapper
/// and its active external-byte owner. The returned scalar remains a capability
/// whose later use must be checked against that same owner ledger.
pub(super) unsafe extern "C" fn prepared_byte_array_contents(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    output: *mut usize,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (published, _) = unsafe { active_bytes(machine, vmctx, reference, descriptor) }?;
        let address = machine
            .external_byte_address(published)
            .map_err(|error| super::arrays::storage_error(error, 0))?;
        unsafe { output.write(address) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_sizeof_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    output: *mut i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (_, len) = unsafe { active_bytes(machine, vmctx, reference, descriptor) }?;
        let len = i64::try_from(len).map_err(|_| RuntimeError::BadPointer)?;
        unsafe { output.write(len) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(e) => super::arrays::array_error(machine, e),
    }
}

pub(super) unsafe extern "C" fn prepared_shrink_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    new_len: i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (published, len) = unsafe { active_bytes(machine, vmctx, reference, descriptor) }?;
        let new_len = usize::try_from(new_len)
            .ok()
            .filter(|&candidate| candidate <= len)
            .ok_or(RuntimeError::ArrayIndexOutOfBounds {
                index: new_len,
                len,
            })?;
        machine
            .shrink_external_payload(published, ExternalStorageKind::Bytes, new_len)
            .map_err(|error| super::arrays::storage_error(error, new_len as i64))
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

fn checked_byte_span_arg(value: i64, len: usize) -> Result<usize, RuntimeError> {
    usize::try_from(value).map_err(|_| RuntimeError::ArrayIndexOutOfBounds { index: value, len })
}

fn byte_range_error(error: ExternalStorageValidationError) -> RuntimeError {
    match error {
        ExternalStorageValidationError::AliasedByteCopy => RuntimeError::AliasedByteCopy,
        ExternalStorageValidationError::IndexOutOfBounds { index, len } => {
            RuntimeError::ArrayIndexOutOfBounds {
                index: i64::try_from(index).unwrap_or(i64::MAX),
                len,
            }
        }
        other => super::arrays::storage_error(other, 0),
    }
}

/// Both wrappers and complete spans are admitted before the owner copies.
/// No collection, callback, or partial write occurs in this host call.
pub(super) unsafe extern "C" fn prepared_copy_bytes(
    vmctx: *mut crate::context::VMContext,
    descriptor: *const ObjectDescriptor,
    source: *mut u8,
    source_offset: i64,
    destination: *mut u8,
    destination_offset: i64,
    count: i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (source, source_len) = unsafe { active_bytes(machine, vmctx, source, descriptor) }?;
        let (destination, destination_len) =
            unsafe { active_bytes(machine, vmctx, destination, descriptor) }?;
        let source_offset = checked_byte_span_arg(source_offset, source_len)?;
        let destination_offset = checked_byte_span_arg(destination_offset, destination_len)?;
        let count = checked_byte_span_arg(count, source_len)?;
        machine
            .copy_external_byte_range(
                source,
                source_offset,
                destination,
                destination_offset,
                count,
            )
            .map_err(byte_range_error)
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

/// Compare authenticated byte spans without allocation or mutation. Aliases
/// are valid; the result slot is published only after complete validation.
pub(super) unsafe extern "C" fn prepared_compare_bytes(
    vmctx: *mut crate::context::VMContext,
    descriptor: *const ObjectDescriptor,
    left: *mut u8,
    left_offset: i64,
    right: *mut u8,
    right_offset: i64,
    count: i64,
    output: *mut i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (left, left_len) = unsafe { active_bytes(machine, vmctx, left, descriptor) }?;
        let (right, right_len) = unsafe { active_bytes(machine, vmctx, right, descriptor) }?;
        let left_offset = checked_byte_span_arg(left_offset, left_len)?;
        let right_offset = checked_byte_span_arg(right_offset, right_len)?;
        let count = checked_byte_span_arg(count, left_len)?;
        let ordering = machine
            .compare_external_byte_ranges(left, left_offset, right, right_offset, count)
            .map_err(byte_range_error)?;
        unsafe { output.write(ordering) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

unsafe fn prepared_read_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    output: *mut i64,
    element: Element,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (published, len) = unsafe { active_bytes(machine, vmctx, reference, descriptor) }?;
        let offset = checked_offset(index, len, element)?;
        let data = unsafe { published.add(8).add(offset) };
        let value = match element {
            Element::Address => (unsafe { data.cast::<u64>().read_unaligned() }) as i64,
            Element::Word8 => i64::from(unsafe { data.read() }),
            Element::Word64 => (unsafe { data.cast::<u64>().read_unaligned() }) as i64,
            Element::Int64 => unsafe { data.cast::<i64>().read_unaligned() },
        };
        unsafe { output.write(value) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(e) => super::arrays::array_error(machine, e),
    }
}

pub(super) unsafe extern "C" fn prepared_read_word8_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    output: *mut i64,
) -> i32 {
    unsafe { prepared_read_bytes(vmctx, reference, descriptor, index, output, Element::Word8) }
}

pub(super) unsafe extern "C" fn prepared_read_int_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    output: *mut i64,
) -> i32 {
    unsafe { prepared_read_bytes(vmctx, reference, descriptor, index, output, Element::Int64) }
}

unsafe fn prepared_write_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    value: i64,
    element: Element,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (published, len) = unsafe { active_bytes(machine, vmctx, reference, descriptor) }?;
        let offset = checked_offset(index, len, element)?;
        match element {
            Element::Address => machine
                .store_external_bytes(published, offset, &value.to_ne_bytes())
                .map_err(|error| super::arrays::storage_error(error, index))?,
            Element::Word8 => machine
                .store_external_bytes(published, offset, &[value as u8])
                .map_err(|error| super::arrays::storage_error(error, index))?,
            Element::Word64 | Element::Int64 => machine
                .store_external_bytes(published, offset, &value.to_ne_bytes())
                .map_err(|error| super::arrays::storage_error(error, index))?,
        }
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(e) => super::arrays::array_error(machine, e),
    }
}

pub(super) unsafe extern "C" fn prepared_write_word8_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    value: i64,
) -> i32 {
    unsafe { prepared_write_bytes(vmctx, reference, descriptor, index, value, Element::Word8) }
}

pub(super) unsafe extern "C" fn prepared_write_int_bytes(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    index: i64,
    value: i64,
) -> i32 {
    unsafe { prepared_write_bytes(vmctx, reference, descriptor, index, value, Element::Int64) }
}

fn owner_value(builder: &mut FunctionBuilder<'_>, descriptor: &ObjectDescriptor) -> Value {
    builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64)
}
pub(super) fn emit_new_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: cranelift_module::FuncId,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let gc = pipeline.module.declare_func_in_func(gc, builder.func);
    let object = crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, descriptor, gc);
    let header = owner_value(builder, descriptor);
    builder.ins().store(MemFlags::trusted(), header, object, 0);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().store(MemFlags::trusted(), zero, object, 8);
    let host = super::arrays::declare_host(builder, pipeline, "prepared_new_bytes", 3)?;
    let call = builder.ins().call(host, &[vmctx, object, arguments[0]]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    let result = builder.ins().bor_imm(object, i64::from(descriptor.tag()));
    builder.declare_value_needs_stack_map(result);
    Ok(vec![result])
}

pub(super) fn emit_resize_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: cranelift_module::FuncId,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    // Reserve may collect and rewrite the old managed wrapper before the host
    // authenticates it. The external replacement is allocated afterward.
    builder.declare_value_needs_stack_map(arguments[0]);
    let gc = pipeline.module.declare_func_in_func(gc, builder.func);
    let object = crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, descriptor, gc);
    let header = owner_value(builder, descriptor);
    builder.ins().store(MemFlags::trusted(), header, object, 0);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().store(MemFlags::trusted(), zero, object, 8);
    let host = super::arrays::declare_host(builder, pipeline, "prepared_resize_bytes", 5)?;
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], header, object, arguments[1]]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    let result = builder.ins().bor_imm(object, i64::from(descriptor.tag()));
    builder.declare_value_needs_stack_map(result);
    Ok(vec![result])
}

pub(super) fn emit_freeze_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_freeze_bytes", 3)?;
    let owner = owner_value(builder, descriptor);
    let call = builder.ins().call(host, &[vmctx, arguments[0], owner]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    builder.declare_value_needs_stack_map(arguments[0]);
    Ok(vec![arguments[0]])
}

pub(super) fn emit_byte_array_contents(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_byte_array_contents", 4)?;
    let owner = owner_value(builder, descriptor);
    let output = super::arrays::output_slot(builder);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], owner, output]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(vec![builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        output,
        0,
    )])
}

pub(super) fn emit_sizeof_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_sizeof_bytes", 4)?;
    let owner = owner_value(builder, descriptor);
    let output = super::arrays::output_slot(builder);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], owner, output]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(vec![builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        output,
        0,
    )])
}

pub(super) fn emit_shrink_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_shrink_bytes", 4)?;
    let owner = owner_value(builder, descriptor);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], owner, arguments[1]]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(Vec::new())
}

pub(super) fn emit_copy_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_copy_bytes", 7)?;
    let owner = owner_value(builder, descriptor);
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
    super::arrays::finish_checked_call(builder, status);
    Ok(Vec::new())
}

pub(super) fn emit_compare_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_compare_bytes", 8)?;
    let owner = owner_value(builder, descriptor);
    let output = super::arrays::output_slot(builder);
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
            output,
        ],
    );
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(vec![builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        output,
        0,
    )])
}

pub(super) fn emit_read_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
    element: Element,
) -> Result<Vec<Value>, super::CompileError> {
    let name = match element {
        Element::Address => "prepared_read_int_bytes",
        Element::Word8 => "prepared_read_word8_bytes",
        Element::Word64 => "prepared_read_int_bytes",
        Element::Int64 => "prepared_read_int_bytes",
    };
    let host = super::arrays::declare_host(builder, pipeline, name, 5)?;
    let owner = owner_value(builder, descriptor);
    let output = super::arrays::output_slot(builder);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], owner, arguments[1], output]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    let value = match element {
        Element::Address => builder
            .ins()
            .load(types::I64, MemFlags::trusted(), output, 0),
        Element::Word8 => builder
            .ins()
            .load(types::I8, MemFlags::trusted(), output, 0),
        Element::Word64 => builder
            .ins()
            .load(types::I64, MemFlags::trusted(), output, 0),
        Element::Int64 => builder
            .ins()
            .load(types::I64, MemFlags::trusted(), output, 0),
    };
    Ok(vec![value])
}

pub(super) fn emit_write_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
    element: Element,
) -> Result<Vec<Value>, super::CompileError> {
    let name = match element {
        Element::Address => "prepared_write_int_bytes",
        Element::Word8 => "prepared_write_word8_bytes",
        Element::Word64 => "prepared_write_int_bytes",
        Element::Int64 => "prepared_write_int_bytes",
    };
    let host = super::arrays::declare_host(builder, pipeline, name, 5)?;
    let owner = owner_value(builder, descriptor);
    let value = match element {
        Element::Address => arguments[2],
        Element::Word8 => builder.ins().uextend(types::I64, arguments[2]),
        Element::Word64 => arguments[2],
        Element::Int64 => arguments[2],
    };
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], owner, arguments[1], value]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::*;

    fn int(value: i64) -> Atom {
        Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    }

    fn byte_program(
        element: Element,
        index: i64,
        length: i64,
        garbage: usize,
        index_word: bool,
    ) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        let (write_name, read_name, rep, value) = match element {
            Element::Address => unreachable!("address indexing has a dedicated fixture"),
            Element::Word8 => (
                "writeWord8Array#",
                "indexWord8Array#",
                RuntimeRep::Word(8),
                Atom::Scalar(ScalarLiteral::Word {
                    bits: 8,
                    bytes: vec![0xe7],
                }),
            ),
            Element::Word64 => (
                "writeWordArray#",
                if index_word {
                    "indexWordArray#"
                } else {
                    "readWordArray#"
                },
                RuntimeRep::Word(64),
                Atom::Scalar(ScalarLiteral::Word {
                    bits: 64,
                    bytes: u64::MAX.to_be_bytes().to_vec(),
                }),
            ),
            Element::Int64 => (
                "writeIntArray#",
                "readIntArray#",
                RuntimeRep::Int(64),
                int(-0x1234567),
            ),
        };
        wire.signatures[0].results = ResultContract::Returns(vec![rep]);
        wire.signatures.extend([
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    rep,
                    RuntimeRep::Void,
                ],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: if element != Element::Word8 && !index_word {
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Void,
                    ]
                } else {
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)]
                },
                results: ResultContract::Returns(vec![rep]),
            },
        ]);
        wire.operations = ["newByteArray#", write_name, read_name]
            .into_iter()
            .enumerate()
            .map(|(id, name)| OperationDecl {
                identity: OperationIdentity::PrimOp(name.into()),
                signature: SignatureId(id as u32 + 1),
            })
            .collect();
        let mut nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![int(length), Atom::Void],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![
                    Atom::Ref(ValueRef::Local(ValueId(100))),
                    int(0),
                    value,
                    Atom::Void,
                ],
            },
            ExprFrame::Operation {
                operation: OperationId(2),
                arguments: if element != Element::Word8 && !index_word {
                    vec![
                        Atom::Ref(ValueRef::Local(ValueId(100))),
                        int(index),
                        Atom::Void,
                    ]
                } else {
                    vec![Atom::Ref(ValueRef::Local(ValueId(100))), int(index)]
                },
            },
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(102)))]),
            ExprFrame::Case {
                scrutinee: 2,
                binder: ValueId(102),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![rep]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 3,
                }],
            },
        ];
        let mut after_write = 4;
        if garbage != 0 {
            wire.constructors.push(ConstructorDecl {
                identity: testing::identity("Bytes", "C"),
                family: testing::identity("Bytes", "T"),
                host_id: tidepool_repr::DataConId(1890),
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
            for n in 0..garbage {
                let construct = nodes.len();
                nodes.push(ExprFrame::Construct {
                    constructor: ConstructorId(0),
                    fields: vec![],
                });
                let case = nodes.len();
                nodes.push(ExprFrame::Case {
                    scrutinee: construct,
                    binder: ValueId(200 + n as u32),
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
        }
        let write_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 1,
            binder: ValueId(101),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: after_write,
            }],
        });
        let new_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(103),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![ValueId(100)],
                body: write_case,
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
        crate::prepared_program::CompiledProgram::compile(&linked).unwrap()
    }

    fn index_address_program(length: i64, index: i64) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures.extend([
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Word(64),
                    RuntimeRep::Void,
                ],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)],
                results: ResultContract::Returns(vec![RuntimeRep::Address]),
            },
        ]);
        wire.operations = vec![
            OperationDecl {
                identity: OperationIdentity::PrimOp("newByteArray#".into()),
                signature: SignatureId(1),
            },
            OperationDecl {
                identity: OperationIdentity::PrimOp("writeWordArray#".into()),
                signature: SignatureId(2),
            },
            OperationDecl {
                identity: OperationIdentity::PrimOp("indexAddrArray#".into()),
                signature: SignatureId(3),
            },
        ];
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![int(length), Atom::Void],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![
                    Atom::Ref(ValueRef::Local(ValueId(100))),
                    int(0),
                    Atom::Scalar(ScalarLiteral::Word {
                        bits: 64,
                        bytes: u64::MAX.to_be_bytes().to_vec(),
                    }),
                    Atom::Void,
                ],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![
                    Atom::Ref(ValueRef::Local(ValueId(100))),
                    int(1),
                    Atom::Scalar(ScalarLiteral::Word {
                        bits: 64,
                        bytes: 0_u64.to_be_bytes().to_vec(),
                    }),
                    Atom::Void,
                ],
            },
            ExprFrame::Operation {
                operation: OperationId(2),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(100))), int(index)],
            },
            ExprFrame::Return(vec![int(0)]),
            ExprFrame::Return(vec![int(1)]),
            ExprFrame::Case {
                scrutinee: 3,
                binder: ValueId(103),
                kind: CaseKind::Primitive(RuntimeRep::Address),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Address]),
                alternatives: vec![
                    Alternative {
                        pattern: AlternativePattern::Literal(ScalarLiteral::NullAddress),
                        binders: vec![],
                        body: 5,
                    },
                    Alternative {
                        pattern: AlternativePattern::Default,
                        binders: vec![],
                        body: 4,
                    },
                ],
            },
            ExprFrame::Case {
                scrutinee: 2,
                binder: ValueId(102),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 6,
                }],
            },
            ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(101),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 7,
                }],
            },
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(104),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(100)],
                    body: 8,
                }],
            },
        ];
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!("fixture entry is nonrecursive")
        };
        let HeapRhs::Function { body, .. } = &mut entry.binding.rhs else {
            unreachable!("fixture entry is a function")
        };
        *body = 9;
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        crate::prepared_program::CompiledProgram::compile(&linked).unwrap()
    }

    #[test]
    fn byte_and_int_roundtrip_through_real_adapter_and_collection() {
        for (element, expected) in [
            (Element::Word8, 0xe7_u64),
            (Element::Word64, u64::MAX),
            (Element::Int64, (-0x1234567_i64) as u64),
        ] {
            let program = byte_program(element, 0, 8, 24, false);
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
            assert!(result.collections >= 2);
            match (element, result.values.as_slice()) {
                (
                    Element::Word8,
                    [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(value))],
                ) => assert_eq!(*value, expected),
                (
                    Element::Word64,
                    [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(value))],
                ) => assert_eq!(*value, expected),
                (
                    Element::Int64,
                    [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(value))],
                ) => assert_eq!(*value as u64, expected),
                _ => panic!("unexpected byte-array result: {:?}", result.values),
            }
        }
        let indexed = byte_program(Element::Word64, 0, 8, 24, true)
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
        assert!(indexed.collections >= 2);
        assert!(matches!(
            indexed.values.as_slice(),
            [tidepool_bridge::Value::Lit(
                tidepool_repr::Literal::LitWord(u64::MAX)
            )]
        ));
    }

    #[test]
    fn address_index_reads_one_machine_word_from_byte_array_owner() {
        let result = index_address_program(16, 1)
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

        for (length, index) in [(16, -1), (16, 2)] {
            let error = index_address_program(length, index)
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
                    if failure.cause
                        == RuntimeError::ArrayIndexOutOfBounds {
                            index,
                            len: length.max(0) as usize / 8,
                        }
            ));
        }
    }

    #[test]
    fn shrink_keeps_written_prefix_for_frozen_snapshot_size_and_read_after_gc() {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![
            RuntimeRep::UnliftedRef,
            RuntimeRep::Int(64),
            RuntimeRep::Word(8),
        ]);
        wire.signatures.extend([
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Word(8),
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
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            },
            Signature {
                arguments: vec![RuntimeRep::UnliftedRef],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
            },
            Signature {
                arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)],
                results: ResultContract::Returns(vec![RuntimeRep::Word(8)]),
            },
        ]);
        wire.operations = [
            "newByteArray#",
            "writeWord8Array#",
            "shrinkMutableByteArray#",
            "unsafeFreezeByteArray#",
            "sizeofByteArray#",
            "indexWord8Array#",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(index as u32 + 1),
        })
        .collect();
        let local = |id| Atom::Ref(ValueRef::Local(ValueId(id)));
        let operation = |id, arguments| ExprFrame::Operation {
            operation: OperationId(id),
            arguments,
        };
        let case = |scrutinee, binder, results: Vec<RuntimeRep>, binders: Vec<ValueId>, body| {
            let kind = if results.len() == 1 && binders.is_empty() {
                CaseKind::Polymorphic
            } else {
                CaseKind::MultiValue
            };
            ExprFrame::Case {
                scrutinee,
                binder: ValueId(binder),
                kind,
                scrutinee_results: ResultContract::Returns(results),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders,
                    body,
                }],
            }
        };
        wire.expressions.nodes = vec![
            operation(0, vec![int(4), Atom::Void]),
            operation(
                1,
                vec![
                    local(100),
                    int(0),
                    Atom::Scalar(ScalarLiteral::Word {
                        bits: 8,
                        bytes: vec![0xe7],
                    }),
                    Atom::Void,
                ],
            ),
            operation(2, vec![local(100), int(1), Atom::Void]),
            operation(3, vec![local(100), Atom::Void]),
            operation(4, vec![local(102)]),
            operation(5, vec![local(102), int(0)]),
            ExprFrame::Return(vec![local(102), local(103), local(104)]),
            case(5, 104, vec![RuntimeRep::Word(8)], vec![], 6),
            case(4, 103, vec![RuntimeRep::Int(64)], vec![], 7),
            case(3, 109, vec![RuntimeRep::UnliftedRef], vec![ValueId(102)], 8),
            case(2, 108, vec![], vec![], 9),
            case(1, 107, vec![], vec![], 10),
            case(
                0,
                106,
                vec![RuntimeRep::UnliftedRef],
                vec![ValueId(100)],
                11,
            ),
        ];
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                *body = 12;
            }
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        let program = crate::prepared_program::CompiledProgram::compile(&linked).unwrap();
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
        assert!(result.collections >= 1);
        assert!(matches!(
            result.values.as_slice(),
            [
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitByteArray(bytes)),
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(1)),
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(0xe7)),
            ] if bytes.as_slice() == [0xe7]
        ));
    }

    #[test]
    fn byte_array_index_failures_are_typed_and_terminal() {
        for (element, index, length, diagnostic_len) in [
            (Element::Word8, -1, 8, 8),
            (Element::Word8, 8, 8, 8),
            (Element::Word64, -1, 8, 1),
            (Element::Word64, 1, 15, 1),
            (Element::Word64, i64::MAX, 8, 1),
            (Element::Int64, 1, 15, 1),
            (Element::Int64, i64::MAX, 8, 1),
        ] {
            let program = byte_program(element, index, length, 0, false);
            let error = program
                .run_entry(
                    ValueId(0),
                    &[],
                    &Default::default(),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap_err();
            assert!(
                matches!(error, crate::prepared_program::ExecutionError::Runtime(failure)
                if failure.cause == RuntimeError::ArrayIndexOutOfBounds { index, len: diagnostic_len }
                    && failure.disposition == crate::machine_state::MachineDisposition::Reusable)
            );
        }
    }

    #[test]
    fn byte_operations_require_exact_ghc_signatures() {
        let sig = |arguments, results| Signature {
            arguments,
            results: ResultContract::Returns(results),
        };
        let op = |name: &str| OperationIdentity::PrimOp(name.into());
        assert_eq!(
            recognize(
                &op("newPinnedByteArray#"),
                &sig(
                    vec![RuntimeRep::Int(64), RuntimeRep::Void],
                    vec![RuntimeRep::UnliftedRef]
                )
            ),
            Some(ByteOperation::New)
        );
        for name in ["byteArrayContents#", "mutableByteArrayContents#"] {
            assert_eq!(
                recognize(
                    &op(name),
                    &sig(vec![RuntimeRep::UnliftedRef], vec![RuntimeRep::Address])
                ),
                Some(ByteOperation::Contents)
            );
            assert!(recognize(
                &op(name),
                &sig(
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
                    vec![RuntimeRep::Address]
                )
            )
            .is_none());
        }
        assert_eq!(
            recognize(
                &op("readWord8Array#"),
                &sig(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Void
                    ],
                    vec![RuntimeRep::Word(8)]
                )
            ),
            Some(ByteOperation::Read(Element::Word8))
        );
        assert!(recognize(
            &op("readWord8Array#"),
            &sig(
                vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Void
                ],
                vec![RuntimeRep::Word(64)]
            )
        )
        .is_none());
        assert_eq!(
            recognize(
                &op("readWordArray#"),
                &sig(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Void
                    ],
                    vec![RuntimeRep::Word(64)]
                )
            ),
            Some(ByteOperation::Read(Element::Word64))
        );
        assert_eq!(
            recognize(
                &op("indexAddrArray#"),
                &sig(
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)],
                    vec![RuntimeRep::Address]
                )
            ),
            Some(ByteOperation::Read(Element::Address))
        );
        assert!(recognize(
            &op("indexAddrArray#"),
            &sig(
                vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Void
                ],
                vec![RuntimeRep::Address]
            )
        )
        .is_none());
        assert_eq!(
            recognize(
                &op("indexWordArray#"),
                &sig(
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)],
                    vec![RuntimeRep::Word(64)]
                )
            ),
            Some(ByteOperation::Read(Element::Word64))
        );
        assert_eq!(
            recognize(
                &op("writeWordArray#"),
                &sig(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Word(64),
                        RuntimeRep::Void
                    ],
                    vec![]
                )
            ),
            Some(ByteOperation::Write(Element::Word64))
        );
        assert!(recognize(
            &op("readWordArray#"),
            &sig(
                vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)],
                vec![RuntimeRep::Word(64)]
            )
        )
        .is_none());
        assert_eq!(
            recognize(
                &op("getSizeofMutableByteArray#"),
                &sig(
                    vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
                    vec![RuntimeRep::Int(64)]
                )
            ),
            Some(ByteOperation::Size)
        );
        assert_eq!(
            recognize(
                &op("shrinkMutableByteArray#"),
                &sig(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Void
                    ],
                    vec![]
                )
            ),
            Some(ByteOperation::Shrink)
        );
        assert!(recognize(
            &op("shrinkMutableByteArray#"),
            &sig(vec![RuntimeRep::UnliftedRef, RuntimeRep::Int(64)], vec![])
        )
        .is_none());
        assert_eq!(
            recognize(
                &op("resizeMutableByteArray#"),
                &sig(
                    vec![
                        RuntimeRep::UnliftedRef,
                        RuntimeRep::Int(64),
                        RuntimeRep::Void
                    ],
                    vec![RuntimeRep::UnliftedRef]
                )
            ),
            Some(ByteOperation::Resize)
        );
        assert!(recognize(
            &op("resizeMutableByteArray#"),
            &sig(
                vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Void
                ],
                vec![]
            )
        )
        .is_none());
        let span_arguments = vec![
            RuntimeRep::UnliftedRef,
            RuntimeRep::Int(64),
            RuntimeRep::UnliftedRef,
            RuntimeRep::Int(64),
            RuntimeRep::Int(64),
        ];
        let mut copy_arguments = span_arguments.clone();
        copy_arguments.push(RuntimeRep::Void);
        assert_eq!(
            recognize(&op("copyByteArray#"), &sig(copy_arguments.clone(), vec![])),
            Some(ByteOperation::Copy)
        );
        assert!(recognize(&op("copyByteArray#"), &sig(span_arguments.clone(), vec![])).is_none());
        assert!(recognize(
            &op("copyByteArray#"),
            &sig(copy_arguments, vec![RuntimeRep::Int(64)])
        )
        .is_none());
        assert_eq!(
            recognize(
                &op("compareByteArrays#"),
                &sig(span_arguments.clone(), vec![RuntimeRep::Int(64)])
            ),
            Some(ByteOperation::Compare)
        );
        assert!(recognize(&op("compareByteArrays#"), &sig(span_arguments, vec![])).is_none());
        assert!(recognize(
            &op("writeIntArray#"),
            &sig(
                vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Int(64)
                ],
                vec![]
            )
        )
        .is_none());
    }

    #[test]
    fn wrong_kind_and_revoked_byte_handles_reject_without_output() {
        use tidepool_repr::execution_schema::{Architecture, Endianness, TargetDescriptor};
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        for kind in [ExternalStorageKind::BoxedArray, ExternalStorageKind::Bytes] {
            let descriptor =
                Arc::new(ObjectDescriptor::external(ExternalStorageKind::Bytes, &target).unwrap());
            let machine = crate::machine_state::MachineState::new();
            let extent = descriptor.allocation_extent() as usize;
            machine
                .install_prepared_buffer(vec![0_u64; extent / 8], vec![descriptor.clone()])
                .unwrap();
            let (start, size) = machine.gc_active_range().unwrap();
            let mut vmctx = unsafe {
                crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
            };
            vmctx.alloc_ptr = unsafe { start.add(extent) };
            vmctx.machine_state = &machine as *const _ as *mut _;
            let reference = (start as usize | usize::from(descriptor.tag())) as *mut u8;
            let payload = machine.allocate_external_storage(kind, 8).unwrap();
            unsafe {
                descriptor.initialize_header(start);
                descriptor
                    .external_payload_slot(start, extent)
                    .unwrap()
                    .write(payload);
            }
            if kind == ExternalStorageKind::Bytes {
                machine.revoke_external_payload(payload, kind).unwrap();
            }
            let mut output = 0x55_i64;
            let status = unsafe {
                prepared_read_word8_bytes(
                    &mut vmctx,
                    reference,
                    Arc::as_ptr(&descriptor),
                    0,
                    &mut output,
                )
            };
            assert_eq!(status, CallStatus::IntegrityFailure as i32);
            assert_eq!(output, 0x55);
            assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
        }
    }

    #[test]
    fn byte_array_contents_authenticates_owner_before_publishing_address() {
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
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::Bytes, &target).unwrap());
        let machine = crate::machine_state::MachineState::new();
        let extent = descriptor.allocation_extent() as usize;
        machine
            .install_prepared_buffer(vec![0_u64; extent / 8], vec![descriptor.clone()])
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        vmctx.alloc_ptr = unsafe { start.add(extent) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        let reference = (start as usize | usize::from(descriptor.tag())) as *mut u8;
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 3)
            .unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(payload);
        }
        let mut output = usize::MAX;
        assert_eq!(
            unsafe {
                prepared_byte_array_contents(
                    &mut vmctx,
                    reference,
                    Arc::as_ptr(&descriptor),
                    &mut output,
                )
            },
            CallStatus::Success as i32
        );
        assert_eq!(output, machine.external_byte_address(payload).unwrap());

        machine
            .revoke_external_payload(payload, ExternalStorageKind::Bytes)
            .unwrap();
        output = usize::MAX;
        assert_eq!(
            unsafe {
                prepared_byte_array_contents(
                    &mut vmctx,
                    reference,
                    Arc::as_ptr(&descriptor),
                    &mut output,
                )
            },
            CallStatus::IntegrityFailure as i32
        );
        assert_eq!(output, usize::MAX);
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
    }

    #[test]
    fn invalid_byte_shrink_preserves_payload_identity_capacity_and_contents() {
        use tidepool_repr::execution_schema::{Architecture, Endianness, TargetDescriptor};
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        for (new_len, revoked) in [(-1, false), (5, false), (1, true)] {
            let descriptor =
                Arc::new(ObjectDescriptor::external(ExternalStorageKind::Bytes, &target).unwrap());
            let machine = crate::machine_state::MachineState::new();
            let extent = descriptor.allocation_extent() as usize;
            machine
                .install_prepared_buffer(vec![0_u64; extent / 8], vec![descriptor.clone()])
                .unwrap();
            let (start, size) = machine.gc_active_range().unwrap();
            let mut vmctx = unsafe {
                crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
            };
            vmctx.alloc_ptr = unsafe { start.add(extent) };
            vmctx.machine_state = &machine as *const _ as *mut _;
            let reference = (start as usize | usize::from(descriptor.tag())) as *mut u8;
            let payload = machine
                .allocate_external_storage(ExternalStorageKind::Bytes, 4)
                .unwrap();
            machine
                .store_external_bytes(payload, 0, &[11, 22, 33, 44])
                .unwrap();
            unsafe {
                descriptor.initialize_header(start);
                descriptor
                    .external_payload_slot(start, extent)
                    .unwrap()
                    .write(payload);
            }
            if revoked {
                machine
                    .revoke_external_payload(payload, ExternalStorageKind::Bytes)
                    .unwrap();
            }
            let capacity = unsafe { payload.sub(8).cast::<u64>().read() };
            let before = unsafe { std::slice::from_raw_parts(payload.add(8), 4) }.to_vec();
            let status = unsafe {
                prepared_shrink_bytes(&mut vmctx, reference, Arc::as_ptr(&descriptor), new_len)
            };
            assert_eq!(
                status,
                if revoked {
                    CallStatus::IntegrityFailure as i32
                } else {
                    CallStatus::LanguageFailure as i32
                }
            );
            assert_eq!(
                machine.take_runtime_error(),
                Some(if revoked {
                    RuntimeError::BadPointer
                } else {
                    RuntimeError::ArrayIndexOutOfBounds {
                        index: new_len,
                        len: 4,
                    }
                })
            );
            assert_eq!(unsafe { payload.sub(8).cast::<u64>().read() }, capacity);
            assert_eq!(unsafe { payload.cast::<u64>().read() }, 4);
            assert_eq!(
                unsafe { std::slice::from_raw_parts(payload.add(8), 4) },
                before.as_slice()
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
            assert_eq!(
                machine
                    .external_payload_view(payload, ExternalStorageKind::Bytes)
                    .unwrap()
                    .logical_len,
                4
            );
        }
    }

    fn size_program(freeze: bool) -> crate::prepared_program::CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Int(64)]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
        });
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
        });
        wire.signatures.push(Signature {
            arguments: if freeze {
                vec![RuntimeRep::UnliftedRef]
            } else {
                vec![RuntimeRep::UnliftedRef, RuntimeRep::Void]
            },
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.operations = [
            "newByteArray#",
            "unsafeFreezeByteArray#",
            if freeze {
                "sizeofByteArray#"
            } else {
                "getSizeofMutableByteArray#"
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(id, name)| OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(id as u32 + 1),
        })
        .collect();
        let mut nodes = vec![ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![int(17), Atom::Void],
        }];
        if freeze {
            nodes.push(ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(100))), Atom::Void],
            });
        }
        let size_node = nodes.len();
        nodes.push(ExprFrame::Operation {
            operation: OperationId(2),
            arguments: if freeze {
                vec![Atom::Ref(ValueRef::Local(ValueId(101)))]
            } else {
                vec![Atom::Ref(ValueRef::Local(ValueId(100))), Atom::Void]
            },
        });
        let return_node = nodes.len();
        nodes.push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
            ValueId(102),
        ))]));
        let size_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: size_node,
            binder: ValueId(102),
            kind: CaseKind::Polymorphic,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: return_node,
            }],
        });
        let after_new = if freeze {
            let freeze_case = nodes.len();
            nodes.push(ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(104),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(101)],
                    body: size_case,
                }],
            });
            freeze_case
        } else {
            size_case
        };
        let new_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(103),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![ValueId(100)],
                body: after_new,
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
        crate::prepared_program::CompiledProgram::compile(&linked).unwrap()
    }

    #[test]
    fn byte_freeze_and_both_size_variants_use_owned_length() {
        for freeze in [false, true] {
            let program = size_program(freeze);
            let result = program
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
            assert!(matches!(
                result.values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(
                    17
                ))]
            ));
        }
    }

    #[test]
    fn byte_array_bounds_use_element_width_without_overflow() {
        assert_eq!(checked_offset(1, 16, Element::Int64), Ok(8));
        assert_eq!(checked_offset(15, 16, Element::Word8), Ok(15));
        assert!(matches!(
            checked_offset(1, 15, Element::Int64),
            Err(RuntimeError::ArrayIndexOutOfBounds { index: 1, len: 1 })
        ));
        assert!(checked_offset(-1, usize::MAX, Element::Word8).is_err());
        assert!(checked_offset(i64::MAX, usize::MAX, Element::Int64).is_err());
    }
}
