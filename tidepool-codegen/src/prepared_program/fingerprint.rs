//! Pinned GHC MD5 foreign calls over authenticated byte capabilities.
//!
//! Hosts snapshot every input into owned Rust storage before calling the C
//! kernel. They never pass a VM address to C and never collect or re-enter.

use std::sync::Arc;

use cranelift_codegen::ir::{types, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_repr::execution_schema::{
    ForeignConvention, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

use super::md5_kernel::{Context, CONTEXT_BYTES};
use super::static_bytes::PinnedBytes;

const DIGEST_BYTES: usize = 16;

pub(super) const INIT_HOST: &str = "prepared_md5_init";
pub(super) const UPDATE_HOST: &str = "prepared_md5_update";
pub(super) const FINAL_HOST: &str = "prepared_md5_final";

pub(super) fn host_functions() -> [(&'static str, *const u8); 3] {
    [
        (INIT_HOST, prepared_md5_init as *const u8),
        (UPDATE_HOST, prepared_md5_update as *const u8),
        (FINAL_HOST, prepared_md5_final as *const u8),
    ]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FingerprintOperation {
    Init,
    Update,
    Final,
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<FingerprintOperation> {
    use RuntimeRep::*;
    let OperationIdentity::Intrinsic {
        symbol,
        convention: ForeignConvention::CCall,
    } = identity
    else {
        return None;
    };
    let (arguments, operation): (&[RuntimeRep], FingerprintOperation) = match symbol.as_str() {
        "__hsbase_MD5Init" => (&[Address, Void], FingerprintOperation::Init),
        "__hsbase_MD5Update" => (
            &[Address, Address, Int(32), Void],
            FingerprintOperation::Update,
        ),
        "__hsbase_MD5Final" => (&[Address, Address, Void], FingerprintOperation::Final),
        _ => return None,
    };
    (signature.arguments == arguments && signature.results == ResultContract::Returns(vec![]))
        .then_some(operation)
}

fn storage_error(
    error: tidepool_heap::external_storage::ExternalStorageValidationError,
) -> crate::host_fns::RuntimeError {
    super::arrays::storage_error(error, 0)
}

fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, crate::host_fns::RuntimeError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|_| crate::host_fns::RuntimeError::HeapOverflow)?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn snapshot_context(
    machine: &crate::machine_state::MachineState,
    address: usize,
) -> Result<Context, crate::host_fns::RuntimeError> {
    let bytes = machine
        .read_external_address(address, CONTEXT_BYTES)
        .map_err(storage_error)?;
    let bytes: [u8; CONTEXT_BYTES] = bytes
        .try_into()
        .map_err(|_| crate::host_fns::RuntimeError::BadPointer)?;
    Ok(Context::from_bytes(bytes))
}

fn snapshot_input(
    machine: &crate::machine_state::MachineState,
    pool: *const PinnedBytes,
    address: usize,
    length: usize,
) -> Result<Vec<u8>, crate::host_fns::RuntimeError> {
    let pinned = unsafe { pool.as_ref() }.and_then(|pool| pool.read_range(address, length));
    match pinned {
        Some(bytes) => copy_bytes(bytes),
        None => machine
            .read_external_address(address, length)
            .map_err(storage_error),
    }
}

fn finish(
    machine: &crate::machine_state::MachineState,
    result: Result<(), crate::host_fns::RuntimeError>,
) -> i32 {
    match result {
        Ok(()) => crate::prepared_control::CallStatus::Success as i32,
        Err(error) => {
            machine.set_first_cause(error);
            machine.prepared_call_status() as i32
        }
    }
}

/// Initialize one already-owned 88-byte context span.
pub(super) unsafe extern "C" fn prepared_md5_init(
    vmctx: *mut crate::context::VMContext,
    context_address: usize,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != crate::prepared_control::CallStatus::Success {
        return status as i32;
    }
    let result = (|| {
        // The snapshot is intentionally unused: obtaining it authenticates the
        // complete output span before the first mutation.
        let _ = snapshot_context(machine, context_address)?;
        machine
            .store_external_address(context_address, &Context::initialized().to_bytes())
            .map_err(storage_error)
    })();
    finish(machine, result)
}

/// Update from either immutable compiled bytes or an owned external byte span.
pub(super) unsafe extern "C" fn prepared_md5_update(
    vmctx: *mut crate::context::VMContext,
    pool: *const PinnedBytes,
    context_address: usize,
    input_address: usize,
    length: i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != crate::prepared_control::CallStatus::Success {
        return status as i32;
    }
    let result = (|| {
        let length = i32::try_from(length)
            .ok()
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(crate::host_fns::RuntimeError::ArrayIndexOutOfBounds {
                index: length,
                len: i32::MAX as usize,
            })?;
        let context = snapshot_context(machine, context_address)?;
        let input = snapshot_input(machine, pool, input_address, length)?;
        let updated = context.update(&input).to_bytes();
        machine
            .store_external_address(context_address, &updated)
            .map_err(storage_error)
    })();
    finish(machine, result)
}

fn disjoint_ranges(left: usize, left_len: usize, right: usize, right_len: usize) -> bool {
    let Some(left_end) = left.checked_add(left_len) else {
        return false;
    };
    let Some(right_end) = right.checked_add(right_len) else {
        return false;
    };
    left_end <= right || right_end <= left
}

/// Finalize into distinct, already-owned context and digest spans.
pub(super) unsafe extern "C" fn prepared_md5_final(
    vmctx: *mut crate::context::VMContext,
    digest_address: usize,
    context_address: usize,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != crate::prepared_control::CallStatus::Success {
        return status as i32;
    }
    let result = (|| {
        let context = snapshot_context(machine, context_address)?;
        let _ = machine
            .read_external_address(digest_address, DIGEST_BYTES)
            .map_err(storage_error)?;
        if !disjoint_ranges(context_address, CONTEXT_BYTES, digest_address, DIGEST_BYTES) {
            return Err(crate::host_fns::RuntimeError::BadPointer);
        }
        let (digest, cleared) = context.finalize();
        machine
            .store_external_address(context_address, &cleared.to_bytes())
            .map_err(storage_error)?;
        machine
            .store_external_address(digest_address, &digest)
            .map_err(storage_error)
    })();
    finish(machine, result)
}

pub(super) fn emit(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    pool: &Arc<PinnedBytes>,
    operation: FingerprintOperation,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let (host_name, parameters) = match operation {
        FingerprintOperation::Init => (INIT_HOST, 2),
        FingerprintOperation::Update => (UPDATE_HOST, 5),
        FingerprintOperation::Final => (FINAL_HOST, 3),
    };
    let host = super::arrays::declare_host(builder, pipeline, host_name, parameters)?;
    let call = match operation {
        FingerprintOperation::Init => builder.ins().call(host, &[vmctx, arguments[0]]),
        FingerprintOperation::Update => {
            let owner = builder
                .ins()
                .iconst(types::I64, Arc::as_ptr(pool) as usize as i64);
            let length = builder.ins().sextend(types::I64, arguments[2]);
            builder
                .ins()
                .call(host, &[vmctx, owner, arguments[0], arguments[1], length])
        }
        FingerprintOperation::Final => builder
            .ins()
            .call(host, &[vmctx, arguments[0], arguments[1]]),
    };
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::machine_state::ExternalStorageKind;
    use tidepool_repr::execution_schema::*;

    fn vmctx(machine: &crate::machine_state::MachineState) -> crate::context::VMContext {
        let mut vmctx = crate::context::VMContext::new(
            std::ptr::null_mut(),
            std::ptr::null(),
            crate::host_fns::gc_trigger,
        );
        vmctx.machine_state = machine as *const _ as *mut _;
        vmctx
    }

    fn bytes(machine: &crate::machine_state::MachineState, length: usize) -> (usize, *mut u8) {
        let published = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, length)
            .expect("test byte allocation");
        let address = machine
            .external_byte_address(published)
            .expect("test byte address");
        (address, published)
    }

    #[test]
    fn hosts_accept_pinned_and_external_inputs() {
        let machine = crate::machine_state::MachineState::new();
        let mut vmctx = vmctx(&machine);
        let (context, _) = bytes(&machine, CONTEXT_BYTES);
        let (external, external_owner) = bytes(&machine, 3);
        machine
            .store_external_bytes(external_owner, 0, b"def")
            .expect("external input initialization");
        let pinned: Arc<[u8]> = Arc::from(&b"abc"[..]);
        let pinned_address = pinned.as_ptr() as usize;
        let pool = PinnedBytes::new(BTreeMap::from([(b"abc".to_vec(), pinned)]));
        let (digest, _) = bytes(&machine, DIGEST_BYTES);

        assert_eq!(
            unsafe { prepared_md5_init(&mut vmctx, context) },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!(
            unsafe { prepared_md5_update(&mut vmctx, &pool, context, pinned_address, 3) },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!(
            unsafe { prepared_md5_update(&mut vmctx, &pool, context, external, 3) },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!(
            unsafe { prepared_md5_final(&mut vmctx, digest, context) },
            crate::prepared_control::CallStatus::Success as i32
        );
        assert_eq!(
            machine
                .read_external_address(digest, DIGEST_BYTES)
                .expect("digest snapshot"),
            [
                0xe8, 0x0b, 0x50, 0x17, 0x09, 0x89, 0x50, 0xfc, 0x58, 0xaa, 0xd8, 0x3c, 0x8c, 0x14,
                0x97, 0x8e
            ]
        );
        assert_eq!(
            machine
                .read_external_address(context, CONTEXT_BYTES)
                .expect("cleared context snapshot"),
            vec![0; CONTEXT_BYTES]
        );
    }

    #[test]
    fn update_rejects_negative_or_short_input_before_context_mutation() {
        for (input, length) in [(usize::MAX, -1), (usize::MAX, 1)] {
            let machine = crate::machine_state::MachineState::new();
            let mut vmctx = vmctx(&machine);
            let (context, _) = bytes(&machine, CONTEXT_BYTES);
            assert_eq!(
                unsafe { prepared_md5_init(&mut vmctx, context) },
                crate::prepared_control::CallStatus::Success as i32
            );
            let before = machine
                .read_external_address(context, CONTEXT_BYTES)
                .expect("context before rejected update");
            assert_ne!(
                unsafe {
                    prepared_md5_update(&mut vmctx, std::ptr::null(), context, input, length)
                },
                crate::prepared_control::CallStatus::Success as i32
            );
            assert_eq!(
                machine
                    .read_external_address(context, CONTEXT_BYTES)
                    .expect("context after rejected update"),
                before
            );
        }
    }

    #[test]
    fn final_rejects_short_or_overlapping_digest_before_context_mutation() {
        for digest_offset in [CONTEXT_BYTES - DIGEST_BYTES, CONTEXT_BYTES] {
            let machine = crate::machine_state::MachineState::new();
            let mut vmctx = vmctx(&machine);
            let (allocation, _) = bytes(&machine, CONTEXT_BYTES + DIGEST_BYTES - 1);
            assert_eq!(
                unsafe { prepared_md5_init(&mut vmctx, allocation) },
                crate::prepared_control::CallStatus::Success as i32
            );
            let before = machine
                .read_external_address(allocation, CONTEXT_BYTES)
                .expect("context before rejected final");
            assert_ne!(
                unsafe { prepared_md5_final(&mut vmctx, allocation + digest_offset, allocation) },
                crate::prepared_control::CallStatus::Success as i32
            );
            assert_eq!(
                machine
                    .read_external_address(allocation, CONTEXT_BYTES)
                    .expect("context after rejected final"),
                before
            );
        }
    }

    #[test]
    fn hosts_preserve_existing_first_cause_without_inspecting_addresses() {
        let machine = crate::machine_state::MachineState::new();
        machine.set_first_cause(crate::host_fns::RuntimeError::Cancelled);
        let mut vmctx = vmctx(&machine);
        assert_eq!(
            unsafe { prepared_md5_final(&mut vmctx, usize::MAX, usize::MAX) },
            crate::prepared_control::CallStatus::Cancelled as i32
        );
        assert_eq!(
            machine.take_runtime_error(),
            Some(crate::host_fns::RuntimeError::Cancelled)
        );
    }

    #[test]
    fn real_adapter_hashes_pinned_bytes_into_external_storage() {
        let mut wire = testing::wire_program();
        use RuntimeRep::*;
        wire.signatures[0].results = ResultContract::Returns(vec![Word(8)]);
        wire.signatures.extend([
            Signature {
                arguments: vec![Int(64), Void],
                results: ResultContract::Returns(vec![UnliftedRef]),
            },
            Signature {
                arguments: vec![UnliftedRef],
                results: ResultContract::Returns(vec![Address]),
            },
            Signature {
                arguments: vec![Address, Void],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![Address, Address, Int(32), Void],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![Address, Address, Void],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![Address, Int(64), Void],
                results: ResultContract::Returns(vec![Word(8)]),
            },
        ]);
        let intrinsic = |symbol: &str, signature| OperationDecl {
            identity: OperationIdentity::Intrinsic {
                symbol: symbol.into(),
                convention: ForeignConvention::CCall,
            },
            signature: SignatureId(signature),
        };
        wire.operations = vec![
            OperationDecl {
                identity: OperationIdentity::PrimOp("newPinnedByteArray#".into()),
                signature: SignatureId(1),
            },
            OperationDecl {
                identity: OperationIdentity::PrimOp("mutableByteArrayContents#".into()),
                signature: SignatureId(2),
            },
            intrinsic("__hsbase_MD5Init", 3),
            intrinsic("__hsbase_MD5Update", 4),
            intrinsic("__hsbase_MD5Final", 5),
            OperationDecl {
                identity: OperationIdentity::PrimOp("readWord8OffAddr#".into()),
                signature: SignatureId(6),
            },
        ];
        let int64 = |value: i64| {
            Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: value.to_be_bytes().to_vec(),
            })
        };
        let int32 = |value: i32| {
            Atom::Scalar(ScalarLiteral::Int {
                bits: 32,
                bytes: value.to_be_bytes().to_vec(),
            })
        };
        let local = |id| Atom::Ref(ValueRef::Local(ValueId(id)));
        let operation = |id, arguments| ExprFrame::Operation {
            operation: OperationId(id),
            arguments,
        };
        let case = |scrutinee, binder, results, binders, body| ExprFrame::Case {
            scrutinee,
            binder: ValueId(binder),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(results),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders,
                body,
            }],
        };
        let mut nodes = vec![
            operation(0, vec![int64(CONTEXT_BYTES as i64), Atom::Void]),
            operation(1, vec![local(100)]),
            operation(0, vec![int64(DIGEST_BYTES as i64), Atom::Void]),
            operation(1, vec![local(102)]),
            operation(2, vec![local(101), Atom::Void]),
            operation(
                3,
                vec![
                    local(101),
                    Atom::Scalar(ScalarLiteral::Bytes(b"abc".to_vec())),
                    int32(3),
                    Atom::Void,
                ],
            ),
            operation(4, vec![local(103), local(101), Atom::Void]),
            operation(5, vec![local(103), int64(0), Atom::Void]),
            ExprFrame::Return(vec![local(104)]),
        ];
        let read = nodes.len();
        nodes.push(case(7, 207, vec![Word(8)], vec![ValueId(104)], 8));
        let final_call = nodes.len();
        nodes.push(case(6, 206, vec![], vec![], read));
        let update = nodes.len();
        nodes.push(case(5, 205, vec![], vec![], final_call));
        let init = nodes.len();
        nodes.push(case(4, 204, vec![], vec![], update));
        let digest_contents = nodes.len();
        nodes.push(case(3, 203, vec![Address], vec![ValueId(103)], init));
        let digest = nodes.len();
        nodes.push(case(
            2,
            202,
            vec![UnliftedRef],
            vec![ValueId(102)],
            digest_contents,
        ));
        let context_contents = nodes.len();
        nodes.push(case(1, 201, vec![Address], vec![ValueId(101)], digest));
        let context = nodes.len();
        nodes.push(case(
            0,
            200,
            vec![UnliftedRef],
            vec![ValueId(100)],
            context_contents,
        ));
        wire.expressions.nodes = nodes;
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!("fixture entry is nonrecursive")
        };
        let HeapRhs::Function { body, .. } = &mut top.binding.rhs else {
            unreachable!("fixture entry is a function")
        };
        *body = context;

        let linked = link_program(
            testing::prepare(wire).expect("valid fingerprint fixture"),
            &MachineImports::default(),
        )
        .expect("linked fingerprint fixture");
        let program =
            super::super::CompiledProgram::compile(&linked).expect("compiled fingerprint fixture");
        let result = program
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .expect("fingerprint execution");
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::Value::Lit(
                tidepool_repr::Literal::LitWord(0x90)
            )]
        ));
    }
}
