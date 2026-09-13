//! Authenticated byte-address primitives. Scalar addresses are capabilities
//! into pinned immutable program bytes or active external byte-array owners;
//! they are never dereferenced directly.

use std::sync::Arc;

use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
use cranelift_codegen::ir::{types, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_repr::execution_schema::{OperationIdentity, ResultContract, RuntimeRep, Signature};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AddressOperation {
    ReadWord8,
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
                && signature.results == ResultContract::Returns(vec![Word(8)]) =>
        {
            Some(AddressOperation::ReadWord8)
        }
        "writeWord8OffAddr#"
            if signature.arguments == [Address, Int(64), Word(8), Void]
                && signature.results == ResultContract::Returns(vec![]) =>
        {
            Some(AddressOperation::WriteWord8)
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

/// Read one byte after authenticating the base address against one retained
/// owner. Immutable program bytes are tried first; the machine ledger is the
/// only fallback and owns all mutable-address dereferences.
pub(super) unsafe extern "C" fn prepared_read_word8_address(
    vmctx: *mut crate::context::VMContext,
    pool: *const super::static_bytes::PinnedBytes,
    address: usize,
    offset: i64,
    output: *mut i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if pool.is_null() || output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let value = if let Some(value) = unsafe { &*pool }.read_byte(address, offset) {
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

pub(super) fn emit_read_word8(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    pool: &Arc<super::static_bytes::PinnedBytes>,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_read_word8_address", 5)?;
    let owner = builder
        .ins()
        .iconst(types::I64, Arc::as_ptr(pool) as usize as i64);
    let output = super::arrays::output_slot(builder);
    let call = builder
        .ins()
        .call(host, &[vmctx, owner, arguments[0], arguments[1], output]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(vec![builder.ins().load(
        types::I8,
        MemFlags::trusted(),
        output,
        0,
    )])
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

pub(super) fn host_functions() -> [(&'static str, *const u8); 2] {
    [
        (
            "prepared_read_word8_address",
            prepared_read_word8_address as *const u8,
        ),
        (
            "prepared_write_word8_address",
            prepared_write_word8_address as *const u8,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tidepool_heap::external_storage::ExternalStorageKind;

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
                &operation("writeWord8OffAddr#"),
                &signature(vec![Address, Int(64), Word(8), Void], vec![])
            ),
            Some(AddressOperation::WriteWord8)
        );
        assert!(recognize(
            &operation("readWord8OffAddr#"),
            &signature(vec![Address, Int(64)], vec![Word(8)])
        )
        .is_none());
        assert!(recognize(
            &operation("writeWord8OffAddr#"),
            &signature(vec![Address, Int(64), Word(64), Void], vec![])
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
        let mut vmctx = vmctx(&machine);
        let mut output = -1;
        assert_eq!(
            unsafe {
                prepared_read_word8_address(
                    &mut vmctx,
                    &pool,
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
            unsafe { prepared_read_word8_address(&mut vmctx, &pool, address + 1, 2, &mut output) },
            CallStatus::Success as i32
        );
        assert_eq!(output, i64::from(b'a'));

        output = 0x55;
        assert_eq!(
            unsafe { prepared_read_word8_address(&mut vmctx, &pool, address + 3, 1, &mut output) },
            CallStatus::LanguageFailure as i32
        );
        assert_eq!(output, 0x55);
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::ArrayIndexOutOfBounds { index: 1, len: 4 })
        );
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
    fn prefix_and_revoked_addresses_each_poison_their_own_machine() {
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::new());
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
                unsafe {
                    prepared_read_word8_address(&mut vmctx, &pool, tested_address, 0, &mut output)
                },
                CallStatus::IntegrityFailure as i32
            );
            assert_eq!(output, 0x55);
            assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
        }
    }

    #[test]
    fn first_cause_prevents_reads_and_writes() {
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::new());
        let machine = crate::machine_state::MachineState::new();
        machine.set_first_cause(RuntimeError::BlackHole);
        let mut vmctx = vmctx(&machine);
        let mut output = 0x55;
        assert_eq!(
            unsafe { prepared_read_word8_address(&mut vmctx, &pool, 1, 0, &mut output) },
            CallStatus::LanguageFailure as i32
        );
        assert_eq!(output, 0x55);
        assert_eq!(
            unsafe { prepared_write_word8_address(&mut vmctx, 1, 0, 1) },
            CallStatus::LanguageFailure as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BlackHole));
    }
}
