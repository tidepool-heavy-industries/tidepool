//! Authenticated byte-address primitives. Scalar addresses are capabilities
//! into pinned immutable program bytes or active external byte-array owners;
//! they are never dereferenced directly.

use super::primitives::returns_exact;
use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
use cranelift_codegen::ir::{types, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_repr::execution_schema::{OperationIdentity, RuntimeRep, Signature};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AddressOperation {
    ReadInt8,
    ReadWord32,
    ReadAddress,
    ReadWord8,
    ReadWideChar,
    WriteWideChar,
    WriteWord8,
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<AddressOperation> {
    use RuntimeRep::*;
    let OperationIdentity::PrimOp(name) = identity else {
        return None;
    };
    match name.as_str() {
        "readWord8OffAddr#"
            if signature.arguments == [Address, Int(64), Void]
                && returns_exact(signature, &[Word(8)]) =>
        {
            Some(AddressOperation::ReadWord8)
        }
        "indexWord8OffAddr#"
            if signature.arguments == [Address, Int(64)]
                && returns_exact(signature, &[Word(8)]) =>
        {
            Some(AddressOperation::ReadWord8)
        }
        "readInt8OffAddr#"
            if signature.arguments == [Address, Int(64), Void]
                && returns_exact(signature, &[Int(8)]) =>
        {
            Some(AddressOperation::ReadInt8)
        }
        "readWord32OffAddr#"
            if signature.arguments == [Address, Int(64), Void]
                && returns_exact(signature, &[Word(32)]) =>
        {
            Some(AddressOperation::ReadWord32)
        }
        "readAddrOffAddr#"
            if signature.arguments == [Address, Int(64), Void]
                && returns_exact(signature, &[Address]) =>
        {
            Some(AddressOperation::ReadAddress)
        }
        "readWideCharOffAddr#"
            if signature.arguments == [Address, Int(64), Void]
                && returns_exact(signature, &[Word(64)]) =>
        {
            Some(AddressOperation::ReadWideChar)
        }
        // The pure `index*` forms read the same element without a state token.
        "indexInt8OffAddr#"
            if signature.arguments == [Address, Int(64)] && returns_exact(signature, &[Int(8)]) =>
        {
            Some(AddressOperation::ReadInt8)
        }
        "indexWord32OffAddr#"
            if signature.arguments == [Address, Int(64)]
                && returns_exact(signature, &[Word(32)]) =>
        {
            Some(AddressOperation::ReadWord32)
        }
        "indexAddrOffAddr#"
            if signature.arguments == [Address, Int(64)]
                && returns_exact(signature, &[Address]) =>
        {
            Some(AddressOperation::ReadAddress)
        }
        "indexWideCharOffAddr#"
            if signature.arguments == [Address, Int(64)]
                && returns_exact(signature, &[Word(64)]) =>
        {
            Some(AddressOperation::ReadWideChar)
        }
        "writeWord8OffAddr#"
            if signature.arguments == [Address, Int(64), Word(8), Void]
                && returns_exact(signature, &[]) =>
        {
            Some(AddressOperation::WriteWord8)
        }
        "writeWideCharOffAddr#"
            if signature.arguments == [Address, Int(64), Word(64), Void]
                && returns_exact(signature, &[]) =>
        {
            Some(AddressOperation::WriteWideChar)
        }
        _ => None,
    }
}

fn address_error(
    error: tidepool_heap::external_storage::ExternalStorageValidationError,
    index: i64,
) -> RuntimeError {
    super::arrays::storage_error(error, index)
}

fn read_address_element<const WIDTH: usize>(
    machine: &crate::machine_state::MachineState,
    address: usize,
    index: i64,
) -> Result<[u8; WIDTH], RuntimeError> {
    let offset = index
        .checked_mul(WIDTH as i64)
        .ok_or(RuntimeError::BadPointer)?;
    if let Some(bytes) = machine.resolve_literal_bytes(|pool| {
        pool.read_range_offset(address, offset, WIDTH)
            .map(<[u8; WIDTH]>::try_from)
    }) {
        return bytes.map_err(|_| RuntimeError::BadPointer);
    }
    machine
        .read_external_address_offset(address, offset, WIDTH)
        .map_err(|error| address_error(error, index))?
        .try_into()
        .map_err(|_| RuntimeError::BadPointer)
}

/// Read one byte after authenticating the base address against one retained
/// owner. Immutable program bytes are tried first; the machine ledger is the
/// only fallback and owns all mutable-address dereferences.
pub(super) unsafe extern "C" fn prepared_read_word8_address(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    offset: i64,
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
        let value = if let Some(value) =
            machine.resolve_literal_bytes(|pool| pool.read_byte(address, offset))
        {
            value
        } else {
            machine
                .read_external_address_offset(address, offset, 1)
                .map_err(|error| address_error(error, offset))?[0]
        };
        unsafe { output.write(i64::from(value)) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_read_int8_address(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    index: i64,
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
        let bytes = read_address_element::<1>(machine, address, index)?;
        unsafe { output.write(i64::from(i8::from_ne_bytes(bytes))) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_read_word32_address(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    index: i64,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let bytes = read_address_element::<4>(machine, address, index)?;
        unsafe { output.write(u64::from(u32::from_ne_bytes(bytes))) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_read_address_address(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    index: i64,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let bytes = read_address_element::<8>(machine, address, index)?;
        unsafe { output.write(u64::from_ne_bytes(bytes)) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

/// Read one native-endian 32-bit character after authenticating the complete
/// four-byte span against the allocation owning the original address.
pub(super) unsafe extern "C" fn prepared_read_wide_char_address(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    index: i64,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let offset = index.checked_mul(4).ok_or(RuntimeError::BadPointer)?;
        let value = if let Some(bytes) = machine.resolve_literal_bytes(|pool| {
            pool.read_range_offset(address, offset, 4)
                .map(<[u8; 4]>::try_from)
        }) {
            u32::from_ne_bytes(bytes.map_err(|_| RuntimeError::BadPointer)?)
        } else {
            let bytes = machine
                .read_external_address_offset(address, offset, 4)
                .map_err(|error| address_error(error, index))?;
            u32::from_ne_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| RuntimeError::BadPointer)?,
            )
        };
        unsafe { output.write(u64::from(value)) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

/// Mutate only an active external byte-array owner. Pinned program bytes and
/// arbitrary numeric addresses are rejected by the machine ledger.
pub(super) unsafe extern "C" fn prepared_write_word8_address(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    offset: i64,
    value: i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    match machine
        .store_external_address_offset(address, offset, &[value as u8])
        .map_err(|error| address_error(error, offset))
    {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) unsafe extern "C" fn prepared_write_wide_char_address(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    index: i64,
    value: u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = index
        .checked_mul(4)
        .ok_or(RuntimeError::BadPointer)
        .and_then(|offset| {
            machine
                .store_external_address_offset(address, offset, &(value as u32).to_ne_bytes())
                .map_err(|error| address_error(error, index))
        });
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) fn emit_read_word8(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_scaled_read(
        builder,
        pipeline,
        vmctx,
        arguments,
        "prepared_read_word8_address",
        types::I8,
    )
}

/// The host always writes the result as a full machine word regardless of the
/// primop's result width, so the slot is loaded at `I64` and narrowed here;
/// loading a narrow type directly from offset 0 would depend on endianness.
fn emit_scaled_read(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
    host_name: &str,
    result_type: cranelift_codegen::ir::Type,
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, host_name, 4)?;
    let output = super::arrays::output_slot(builder);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], arguments[1], output]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    let word = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), output, 0);
    Ok(vec![if result_type == types::I64 {
        word
    } else {
        builder.ins().ireduce(result_type, word)
    }])
}

pub(super) fn emit_read_int8(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_scaled_read(
        builder,
        pipeline,
        vmctx,
        arguments,
        "prepared_read_int8_address",
        types::I8,
    )
}

pub(super) fn emit_read_word32(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_scaled_read(
        builder,
        pipeline,
        vmctx,
        arguments,
        "prepared_read_word32_address",
        types::I32,
    )
}

pub(super) fn emit_read_address(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_scaled_read(
        builder,
        pipeline,
        vmctx,
        arguments,
        "prepared_read_address_address",
        types::I64,
    )
}

pub(super) fn emit_read_wide_char(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    emit_scaled_read(
        builder,
        pipeline,
        vmctx,
        arguments,
        "prepared_read_wide_char_address",
        types::I64,
    )
}

pub(super) fn emit_write_word8(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_write_word8_address", 4)?;
    let value = builder.ins().uextend(types::I64, arguments[2]);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], arguments[1], value]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(Vec::new())
}

pub(super) fn emit_write_wide_char(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host =
        super::arrays::declare_host(builder, pipeline, "prepared_write_wide_char_address", 4)?;
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], arguments[1], arguments[2]]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(Vec::new())
}

pub(super) fn host_functions() -> [(&'static str, *const u8); 7] {
    [
        (
            "prepared_read_word8_address",
            prepared_read_word8_address as *const u8,
        ),
        (
            "prepared_read_wide_char_address",
            prepared_read_wide_char_address as *const u8,
        ),
        (
            "prepared_read_int8_address",
            prepared_read_int8_address as *const u8,
        ),
        (
            "prepared_read_word32_address",
            prepared_read_word32_address as *const u8,
        ),
        (
            "prepared_read_address_address",
            prepared_read_address_address as *const u8,
        ),
        (
            "prepared_write_word8_address",
            prepared_write_word8_address as *const u8,
        ),
        (
            "prepared_write_wide_char_address",
            prepared_write_wide_char_address as *const u8,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tidepool_heap::external_storage::ExternalStorageKind;
    use tidepool_repr::execution_schema::ResultContract;

    fn vmctx(machine: &crate::machine_state::MachineState) -> crate::context::VMContext {
        let mut vmctx = crate::context::VMContext::new(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            crate::host_fns::gc_trigger,
        );
        vmctx.machine_state = machine as *const _ as *mut _;
        vmctx
    }

    #[test]
    fn recognizes_only_exact_address_signatures() {
        use RuntimeRep::*;
        let signature = |arguments, results| Signature {
            arguments,
            results: ResultContract::Returns(results),
        };
        let operation = |name: &str| OperationIdentity::PrimOp(name.into());
        assert_eq!(
            recognize(
                &operation("readWord8OffAddr#"),
                &signature(vec![Address, Int(64), Void], vec![Word(8)])
            ),
            Some(AddressOperation::ReadWord8)
        );
        assert_eq!(
            recognize(
                &operation("readInt8OffAddr#"),
                &signature(vec![Address, Int(64), Void], vec![Int(8)])
            ),
            Some(AddressOperation::ReadInt8)
        );
        assert_eq!(
            recognize(
                &operation("readWord32OffAddr#"),
                &signature(vec![Address, Int(64), Void], vec![Word(32)])
            ),
            Some(AddressOperation::ReadWord32)
        );
        assert_eq!(
            recognize(
                &operation("readAddrOffAddr#"),
                &signature(vec![Address, Int(64), Void], vec![Address])
            ),
            Some(AddressOperation::ReadAddress)
        );
        assert_eq!(
            recognize(
                &operation("writeWord8OffAddr#"),
                &signature(vec![Address, Int(64), Word(8), Void], vec![])
            ),
            Some(AddressOperation::WriteWord8)
        );
        assert_eq!(
            recognize(
                &operation("writeWideCharOffAddr#"),
                &signature(vec![Address, Int(64), Word(64), Void], vec![])
            ),
            Some(AddressOperation::WriteWideChar)
        );
        assert_eq!(
            recognize(
                &operation("readWideCharOffAddr#"),
                &signature(vec![Address, Int(64), Void], vec![Word(64)])
            ),
            Some(AddressOperation::ReadWideChar)
        );
        assert_eq!(
            recognize(
                &operation("indexWord8OffAddr#"),
                &signature(vec![Address, Int(64)], vec![Word(8)])
            ),
            Some(AddressOperation::ReadWord8)
        );
        // Wrong argument reps for the pure index form: no state token slot to
        // confuse with, so a non-matching second argument must simply fail.
        assert!(recognize(
            &operation("indexWord8OffAddr#"),
            &signature(vec![Address, Int(32)], vec![Word(8)])
        )
        .is_none());
        // Wrong result rep: same arguments as the admitted pair, wider result.
        assert!(recognize(
            &operation("indexWord8OffAddr#"),
            &signature(vec![Address, Int(64)], vec![Word(64)])
        )
        .is_none());
        // A NoSuccess contract never matches any Returns-shaped arm.
        assert!(recognize(
            &operation("indexWord8OffAddr#"),
            &Signature {
                arguments: vec![Address, Int(64)],
                results: ResultContract::NoSuccess,
            }
        )
        .is_none());
        // The state-threaded shape belongs to readWord8OffAddr#, not this
        // pure spelling: indexWord8OffAddr# never carries a Void token.
        assert!(recognize(
            &operation("indexWord8OffAddr#"),
            &signature(vec![Address, Int(64), Void], vec![Word(8)])
        )
        .is_none());
        assert!(recognize(
            &operation("readWord8OffAddr#"),
            &signature(vec![Address, Int(64)], vec![Word(8)])
        )
        .is_none());
        assert!(recognize(
            &operation("readWord32OffAddr#"),
            &signature(vec![Address, Int(64), Void], vec![Word(64)])
        )
        .is_none());
        assert!(recognize(
            &operation("writeWideCharOffAddr#"),
            &signature(vec![Address, Int(64), Word(32), Void], vec![])
        )
        .is_none());
        assert!(recognize(
            &operation("writeWord8OffAddr#"),
            &signature(vec![Address, Int(64), Word(64), Void], vec![])
        )
        .is_none());
        assert!(recognize(
            &operation("readWideCharOffAddr#"),
            &signature(vec![Address, Int(64), Void], vec![Word(32)])
        )
        .is_none());
        assert!(recognize(
            &operation("readWideCharOffAddr#"),
            &signature(vec![Address, Int(64)], vec![Word(64)])
        )
        .is_none());
    }

    #[test]
    fn reads_pinned_and_mutable_owner_bytes() {
        let pinned: Arc<[u8]> = Arc::from(&b"fixed\0"[..]);
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::from([(
            b"fixed".to_vec(),
            pinned.clone(),
        )]));
        let machine = crate::machine_state::MachineState::new();
        machine.absorb_interned_bytes(&Arc::new(pool));
        let mut vmctx = vmctx(&machine);
        let mut output = -1;
        assert_eq!(
            unsafe {
                prepared_read_word8_address(
                    &mut vmctx,
                    pinned.as_ptr() as usize + 3,
                    -2,
                    &mut output,
                )
            },
            CallStatus::Success as i32
        );
        assert_eq!(output, i64::from(b'i'));

        let published = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        machine.store_external_bytes(published, 0, b"data").unwrap();
        let address = machine.external_byte_address(published).unwrap();
        assert_eq!(
            unsafe { prepared_read_word8_address(&mut vmctx, address + 1, 2, &mut output) },
            CallStatus::Success as i32
        );
        assert_eq!(output, i64::from(b'a'));

        output = 0x55;
        assert_eq!(
            unsafe { prepared_read_word8_address(&mut vmctx, address + 3, 1, &mut output) },
            CallStatus::LanguageFailure as i32
        );
        assert_eq!(output, 0x55);
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::ArrayIndexOutOfBounds { index: 1, len: 4 })
        );
    }

    #[test]
    fn scaled_reads_preserve_width_signedness_and_address_bits() {
        let address_bits = 0x1234_5678_9abc_def0_u64;
        let mut bytes = vec![0x80, 0, 0, 0];
        bytes.extend(0xf123_4567_u32.to_ne_bytes());
        bytes.extend(address_bits.to_ne_bytes());
        let pinned: Arc<[u8]> = Arc::from(bytes);
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::from([(
            b"scaled".to_vec(),
            pinned.clone(),
        )]));
        let machine = crate::machine_state::MachineState::new();
        machine.absorb_interned_bytes(&Arc::new(pool));
        let mut vmctx = vmctx(&machine);

        let mut signed = 0;
        assert_eq!(
            unsafe {
                prepared_read_int8_address(&mut vmctx, pinned.as_ptr() as usize, 0, &mut signed)
            },
            CallStatus::Success as i32
        );
        assert_eq!(signed, -128);

        let mut word = 0;
        assert_eq!(
            unsafe {
                prepared_read_word32_address(
                    &mut vmctx,
                    pinned.as_ptr() as usize + 8,
                    -1,
                    &mut word,
                )
            },
            CallStatus::Success as i32
        );
        assert_eq!(word, 0xf123_4567);

        let mut address = 0;
        assert_eq!(
            unsafe {
                prepared_read_address_address(
                    &mut vmctx,
                    pinned.as_ptr() as usize + 8,
                    0,
                    &mut address,
                )
            },
            CallStatus::Success as i32
        );
        assert_eq!(address, address_bits);
    }

    #[test]
    fn scaled_reads_reject_end_and_cross_owner_without_output() {
        for cross_owner in [false, true] {
            let machine = crate::machine_state::MachineState::new();
            let first = machine
                .allocate_external_storage(ExternalStorageKind::Bytes, 8)
                .unwrap();
            let second = machine
                .allocate_external_storage(ExternalStorageKind::Bytes, 8)
                .unwrap();
            let first_address = machine.external_byte_address(first).unwrap();
            let second_address = machine.external_byte_address(second).unwrap();
            let index = if cross_owner {
                let delta = second_address as i128 - first_address as i128;
                assert_eq!(delta % 8, 0);
                i64::try_from(delta / 8).unwrap()
            } else {
                1
            };
            let mut output = 0xfeed_face_dead_beef;
            let mut vmctx = vmctx(&machine);
            assert_ne!(
                unsafe {
                    prepared_read_address_address(&mut vmctx, first_address, index, &mut output)
                },
                CallStatus::Success as i32
            );
            assert_eq!(output, 0xfeed_face_dead_beef);
            assert!(machine.take_runtime_error().is_some());
        }
    }

    #[test]
    fn mutable_write_and_all_address_rejections_are_atomic() {
        let pinned: Arc<[u8]> = Arc::from(&b"static\0"[..]);
        let machine = crate::machine_state::MachineState::new();
        let published = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 3)
            .unwrap();
        machine.store_external_bytes(published, 0, b"abc").unwrap();
        let address = machine.external_byte_address(published).unwrap();
        let mut vmctx = vmctx(&machine);

        assert_eq!(
            unsafe { prepared_write_word8_address(&mut vmctx, address + 2, -1, b'Z'.into()) },
            CallStatus::Success as i32
        );
        assert_eq!(machine.copy_external_bytes(published).unwrap(), b"aZc");

        for offset in [3, i64::MAX] {
            assert_eq!(machine.take_runtime_error(), None);
            let before = machine.copy_external_bytes(published).unwrap();
            let status =
                unsafe { prepared_write_word8_address(&mut vmctx, address, offset, b'x'.into()) };
            assert_ne!(status, CallStatus::Success as i32);
            assert_eq!(machine.copy_external_bytes(published).unwrap(), before);
            assert!(machine.take_runtime_error().is_some());
        }

        assert_eq!(
            unsafe {
                prepared_write_word8_address(&mut vmctx, pinned.as_ptr() as usize, 0, b'x'.into())
            },
            CallStatus::IntegrityFailure as i32
        );
        assert_eq!(pinned.as_ref(), b"static\0");
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
    }

    #[test]
    fn wide_char_write_stores_only_low_word_in_external_bytes() {
        let machine = crate::machine_state::MachineState::new();
        let published = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 8)
            .unwrap();
        machine.store_external_bytes(published, 0, &[0; 8]).unwrap();
        let address = machine.external_byte_address(published).unwrap();
        let mut external_vmctx = vmctx(&machine);
        assert_eq!(
            unsafe {
                prepared_write_wide_char_address(
                    &mut external_vmctx,
                    address + 8,
                    -1,
                    0xaaaa_bbbb_f123_4567,
                )
            },
            CallStatus::Success as i32
        );
        assert_eq!(
            machine.copy_external_bytes(published).unwrap(),
            [0, 0, 0, 0]
                .into_iter()
                .chain(0xf123_4567_u32.to_ne_bytes())
                .collect::<Vec<_>>()
        );

        let before = machine.copy_external_bytes(published).unwrap();
        assert_ne!(
            unsafe { prepared_write_wide_char_address(&mut external_vmctx, address, 2, 1) },
            CallStatus::Success as i32
        );
        assert_eq!(machine.copy_external_bytes(published).unwrap(), before);

        let pinned: Arc<[u8]> = Arc::from(&b"static\0"[..]);
        let static_machine = crate::machine_state::MachineState::new();
        let mut static_vmctx = vmctx(&static_machine);
        assert_eq!(
            unsafe {
                prepared_write_wide_char_address(&mut static_vmctx, pinned.as_ptr() as usize, 0, 1)
            },
            CallStatus::IntegrityFailure as i32
        );
        assert_eq!(pinned.as_ref(), b"static\0");
        assert_eq!(
            static_machine.take_runtime_error(),
            Some(RuntimeError::BadPointer)
        );
    }

    #[test]
    fn wide_char_reads_native_endian_static_and_external_spans() {
        let expected = 0x10_ff80_u32;
        let mut pinned_bytes = vec![0xaa];
        pinned_bytes.extend(expected.to_ne_bytes());
        pinned_bytes.push(0xbb);
        let pinned: Arc<[u8]> = Arc::from(pinned_bytes);
        let pinned_address = pinned.as_ptr() as usize + 1;
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::from([(
            b"wide".to_vec(),
            pinned,
        )]));
        let machine = crate::machine_state::MachineState::new();
        machine.absorb_interned_bytes(&Arc::new(pool));
        let mut vmctx = vmctx(&machine);
        let mut output = u64::MAX;
        assert_eq!(
            unsafe { prepared_read_wide_char_address(&mut vmctx, pinned_address, 0, &mut output) },
            CallStatus::Success as i32
        );
        assert_eq!(output, u64::from(expected));

        let published = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 12)
            .unwrap();
        let mut external = Vec::new();
        external.extend(expected.to_ne_bytes());
        external.extend(0x1122_3344_u32.to_ne_bytes());
        external.extend(0x5566_7788_u32.to_ne_bytes());
        machine
            .store_external_bytes(published, 0, &external)
            .unwrap();
        let address = machine.external_byte_address(published).unwrap();
        output = u64::MAX;
        assert_eq!(
            unsafe { prepared_read_wide_char_address(&mut vmctx, address + 8, -2, &mut output) },
            CallStatus::Success as i32
        );
        assert_eq!(output, u64::from(expected));
    }

    #[test]
    fn wide_char_rejects_overflow_short_and_cross_owner_spans_before_output() {
        for rejection in 0..3 {
            let machine = crate::machine_state::MachineState::new();
            let first = machine
                .allocate_external_storage(ExternalStorageKind::Bytes, 6)
                .unwrap();
            let second = machine
                .allocate_external_storage(ExternalStorageKind::Bytes, 8)
                .unwrap();
            machine.store_external_bytes(first, 0, b"abcdef").unwrap();
            machine
                .store_external_bytes(second, 0, b"12345678")
                .unwrap();
            let first_address = machine.external_byte_address(first).unwrap();
            let second_address = machine.external_byte_address(second).unwrap();
            let mut vmctx = vmctx(&machine);
            let index = match rejection {
                0 => 1,
                1 => i64::MAX,
                _ => {
                    let delta = second_address as i128 - first_address as i128;
                    assert_eq!(delta % 4, 0);
                    i64::try_from(delta / 4).unwrap()
                }
            };
            let mut output = 0xfeed_face_u64;
            let status = unsafe {
                prepared_read_wide_char_address(&mut vmctx, first_address, index, &mut output)
            };
            assert_ne!(status, CallStatus::Success as i32);
            assert_eq!(output, 0xfeed_face);
            assert!(machine.take_runtime_error().is_some());
        }
    }

    #[test]
    fn generated_wide_char_adapter_returns_a_zero_extended_word() {
        use std::sync::atomic::AtomicBool;
        use tidepool_repr::execution_schema::{testing, *};

        let expected = 0xf123_4567_u32;
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Word(64)]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Address, RuntimeRep::Int(64), RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::Word(64)]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp("readWideCharOffAddr#".into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes = vec![ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![
                Atom::Scalar(ScalarLiteral::Bytes(expected.to_ne_bytes().to_vec())),
                Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 0_i64.to_be_bytes().to_vec(),
                }),
                Atom::Void,
            ],
        }];
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!("fixture entry is nonrecursive")
        };
        let HeapRhs::Function { body, .. } = &mut entry.binding.rhs else {
            unreachable!("fixture entry is a function")
        };
        *body = 0;

        let prepared = testing::prepare(wire).unwrap();
        let linked = link_program(prepared, &MachineImports::default()).unwrap();
        let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
        let result = compiled
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::Value::Lit(
                tidepool_repr::Literal::LitWord(value)
            )] if *value == u64::from(expected)
        ));
    }

    fn run_static_read_adapter(
        name: &str,
        result_rep: RuntimeRep,
        bytes: Vec<u8>,
    ) -> tidepool_bridge::Value {
        use std::sync::atomic::AtomicBool;
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![result_rep]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Address, RuntimeRep::Int(64), RuntimeRep::Void],
            results: ResultContract::Returns(vec![result_rep]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![
                Atom::Scalar(ScalarLiteral::Bytes(bytes)),
                Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 0_i64.to_be_bytes().to_vec(),
                }),
                Atom::Void,
            ],
        };
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
        let mut values = compiled
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
            .values;
        assert_eq!(values.len(), 1);
        values.pop().unwrap()
    }

    #[test]
    fn generated_scaled_read_adapters_preserve_signed_and_unsigned_widths() {
        assert!(matches!(
            run_static_read_adapter("readInt8OffAddr#", RuntimeRep::Int(8), vec![0x80]),
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-128))
        ));
        assert!(matches!(
            run_static_read_adapter(
                "readWord32OffAddr#",
                RuntimeRep::Word(32),
                0xf123_4567_u32.to_ne_bytes().to_vec()
            ),
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(0xf123_4567))
        ));
    }

    #[test]
    fn prefix_and_revoked_addresses_each_poison_their_own_machine() {
        for revoked in [false, true] {
            let machine = crate::machine_state::MachineState::new();
            let published = machine
                .allocate_external_storage(ExternalStorageKind::Bytes, 3)
                .unwrap();
            let address = machine.external_byte_address(published).unwrap();
            if revoked {
                machine
                    .revoke_external_payload(published, ExternalStorageKind::Bytes)
                    .unwrap();
            }
            let mut vmctx = vmctx(&machine);
            let mut output = 0x55;
            let tested_address = if revoked { address } else { published as usize };
            assert_eq!(
                unsafe { prepared_read_word8_address(&mut vmctx, tested_address, 0, &mut output) },
                CallStatus::IntegrityFailure as i32
            );
            assert_eq!(output, 0x55);
            assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
        }
    }

    #[test]
    fn first_cause_prevents_reads_and_writes() {
        let machine = crate::machine_state::MachineState::new();
        machine.set_first_cause(RuntimeError::BlackHole);
        let mut vmctx = vmctx(&machine);
        let mut output = 0x55;
        assert_eq!(
            unsafe { prepared_read_word8_address(&mut vmctx, 1, 0, &mut output) },
            CallStatus::LanguageFailure as i32
        );
        assert_eq!(output, 0x55);
        assert_eq!(
            unsafe { prepared_write_word8_address(&mut vmctx, 1, 0, 1) },
            CallStatus::LanguageFailure as i32
        );
        let mut wide_output = 0x55;
        assert_eq!(
            unsafe { prepared_read_wide_char_address(&mut vmctx, 1, 0, &mut wide_output) },
            CallStatus::LanguageFailure as i32
        );
        assert_eq!(wide_output, 0x55);
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BlackHole));
    }
}
