//! text's byte-search C call over descriptor-backed byte arrays. The host
//! authenticates the managed span through the external-storage ledger before
//! reading a single byte; it never dereferences an unauthenticated address.

use cranelift_codegen::ir::{types, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_heap::{
    execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind,
};
use tidepool_repr::execution_schema::{
    ForeignConvention, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

use crate::{host_fns::RuntimeError, prepared_control::CallStatus};

pub(super) const MEMCHR_HOST: &str = "prepared_text_memchr";

pub(super) fn host_functions() -> [(&'static str, *const u8); 1] {
    [(MEMCHR_HOST, prepared_text_memchr as *const u8)]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TextSearchOperation {
    Memchr,
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<TextSearchOperation> {
    use RuntimeRep::*;
    let OperationIdentity::Intrinsic {
        symbol,
        convention: ForeignConvention::CCall,
    } = identity
    else {
        return None;
    };
    if symbol != "_hs_text_memchr" {
        return None;
    }
    (signature.arguments == [UnliftedRef, Word(64), Word(64), Word(8), Void]
        && signature.results == ResultContract::Returns(vec![Int(64)]))
    .then_some(TextSearchOperation::Memchr)
}

/// The wire span arguments are unsigned; a value the ledger span cannot hold
/// is a bounds failure against the authenticated length, not an address error.
fn checked_word_span_arg(value: u64, len: usize) -> Result<usize, RuntimeError> {
    usize::try_from(value).map_err(|_| RuntimeError::ArrayIndexOutOfBounds {
        index: i64::try_from(value).unwrap_or(i64::MAX),
        len,
    })
}

/// Search an authenticated byte-array span for one needle byte. Read-only and
/// noncollecting; the result is published only after the complete span
/// validates, and the returned index is relative to the span's start (-1 when
/// the needle is absent).
pub(super) unsafe extern "C" fn prepared_text_memchr(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    offset: u64,
    length: u64,
    needle: i64,
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
        let (published, len) = unsafe {
            super::arrays::active_payload(
                machine,
                vmctx,
                reference,
                descriptor,
                ExternalStorageKind::Bytes,
            )
        }?;
        let offset = checked_word_span_arg(offset, len)?;
        let count = checked_word_span_arg(length, len)?;
        let index = machine
            .find_external_byte(published, offset, count, needle as u8)
            .map_err(super::byte_arrays::byte_range_error)?;
        unsafe { output.write(index) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) fn emit_memchr(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, MEMCHR_HOST, 7)?;
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let needle = builder.ins().uextend(types::I64, arguments[3]);
    let output = super::arrays::output_slot(builder);
    let call = builder.ins().call(
        host,
        &[
            vmctx,
            arguments[0],
            owner,
            arguments[1],
            arguments[2],
            needle,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepared_program::{ExecutionError, RunOptions};
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::{testing, *};

    fn sig(arguments: Vec<RuntimeRep>, results: Vec<RuntimeRep>) -> Signature {
        Signature {
            arguments,
            results: ResultContract::Returns(results),
        }
    }

    #[test]
    fn memchr_requires_the_exact_intrinsic_signature() {
        use RuntimeRep::*;
        let identity = OperationIdentity::Intrinsic {
            symbol: "_hs_text_memchr".into(),
            convention: ForeignConvention::CCall,
        };
        let exact = sig(
            vec![UnliftedRef, Word(64), Word(64), Word(8), Void],
            vec![Int(64)],
        );
        assert_eq!(
            recognize(&identity, &exact),
            Some(TextSearchOperation::Memchr)
        );
        for wrong in [
            sig(
                vec![Address, Word(64), Word(64), Word(8), Void],
                vec![Int(64)],
            ),
            sig(
                vec![UnliftedRef, Int(64), Int(64), Word(8), Void],
                vec![Int(64)],
            ),
            sig(
                vec![UnliftedRef, Word(64), Word(64), Word(64), Void],
                vec![Int(64)],
            ),
            sig(
                vec![UnliftedRef, Word(64), Word(64), Word(8)],
                vec![Int(64)],
            ),
            sig(
                vec![UnliftedRef, Word(64), Word(64), Word(8), Void],
                vec![Word(64)],
            ),
            Signature {
                arguments: vec![UnliftedRef, Word(64), Word(64), Word(8), Void],
                results: ResultContract::NoSuccess,
            },
        ] {
            assert_eq!(recognize(&identity, &wrong), None);
        }
        assert_eq!(
            recognize(&OperationIdentity::PrimOp("_hs_text_memchr".into()), &exact),
            None
        );
    }

    fn memchr_wire(offset: u64, length: u64, needle: u8) -> WireProgram {
        let mut wire = testing::wire_program();
        use RuntimeRep::{Int, UnliftedRef, Void, Word};
        wire.signatures[0].results = ResultContract::Returns(vec![Int(64)]);
        wire.signatures.extend([
            Signature {
                arguments: vec![Int(64), Void],
                results: ResultContract::Returns(vec![UnliftedRef]),
            },
            Signature {
                arguments: vec![UnliftedRef, Int(64), Word(8), Void],
                results: ResultContract::Returns(vec![]),
            },
            Signature {
                arguments: vec![UnliftedRef, Word(64), Word(64), Word(8), Void],
                results: ResultContract::Returns(vec![Int(64)]),
            },
        ]);
        wire.operations = vec![
            OperationDecl {
                identity: OperationIdentity::PrimOp("newByteArray#".into()),
                signature: SignatureId(1),
            },
            OperationDecl {
                identity: OperationIdentity::PrimOp("writeWord8Array#".into()),
                signature: SignatureId(2),
            },
            OperationDecl {
                identity: OperationIdentity::Intrinsic {
                    symbol: "_hs_text_memchr".into(),
                    convention: ForeignConvention::CCall,
                },
                signature: SignatureId(3),
            },
        ];
        let int = |value: i64| {
            Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: value.to_be_bytes().to_vec(),
            })
        };
        let word64 = |value: u64| {
            Atom::Scalar(ScalarLiteral::Word {
                bits: 64,
                bytes: value.to_be_bytes().to_vec(),
            })
        };
        let byte = |value: u8| {
            Atom::Scalar(ScalarLiteral::Word {
                bits: 8,
                bytes: vec![value],
            })
        };
        let local = |id| Atom::Ref(ValueRef::Local(ValueId(id)));
        let operation = |id, arguments| ExprFrame::Operation {
            operation: OperationId(id),
            arguments,
        };
        let case = |scrutinee, binder, kind, results, binders, body| ExprFrame::Case {
            scrutinee,
            binder: ValueId(binder),
            kind,
            scrutinee_results: ResultContract::Returns(results),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders,
                body,
            }],
        };
        wire.expressions.nodes = vec![
            operation(0, vec![int(4), Atom::Void]),
            operation(1, vec![local(100), int(0), byte(b'a'), Atom::Void]),
            operation(1, vec![local(100), int(1), byte(b'b'), Atom::Void]),
            operation(1, vec![local(100), int(2), byte(b'c'), Atom::Void]),
            operation(1, vec![local(100), int(3), byte(b'b'), Atom::Void]),
            operation(
                2,
                vec![
                    local(100),
                    word64(offset),
                    word64(length),
                    byte(needle),
                    Atom::Void,
                ],
            ),
            ExprFrame::Return(vec![local(101)]),
            case(
                5,
                101,
                CaseKind::Polymorphic,
                vec![RuntimeRep::Int(64)],
                vec![],
                6,
            ),
            case(4, 204, CaseKind::MultiValue, vec![], vec![], 7),
            case(3, 203, CaseKind::MultiValue, vec![], vec![], 8),
            case(2, 202, CaseKind::MultiValue, vec![], vec![], 9),
            case(1, 201, CaseKind::MultiValue, vec![], vec![], 10),
            case(
                0,
                200,
                CaseKind::MultiValue,
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
        wire
    }

    fn compile(wire: WireProgram) -> crate::prepared_program::CompiledProgram {
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        crate::prepared_program::CompiledProgram::compile(
            &linked,
            crate::prepared_program::TopSlotBase::ZERO,
        )
        .unwrap()
    }

    #[test]
    fn memchr_real_adapter_reports_offsets_relative_to_the_span_start() {
        // The array holds [a, b, c, b].
        for (offset, length, needle, expected) in
            [(1, 3, b'c', 1), (0, 4, b'z', -1), (2, 0, b'c', -1)]
        {
            let program = compile(memchr_wire(offset, length, needle));
            let result = program
                .run_entry(
                    ValueId(0),
                    &[],
                    &RunOptions::default(),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
            assert!(matches!(
                result.values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(index))]
                    if *index == expected
            ));
        }
    }

    #[test]
    fn memchr_real_adapter_rejects_spans_past_the_extent_without_a_result() {
        for (offset, length) in [(2, 3), (5, 1), (0, u64::MAX)] {
            let program = compile(memchr_wire(offset, length, b'a'));
            let error = program
                .run_entry(
                    ValueId(0),
                    &[],
                    &RunOptions::default(),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap_err();
            assert!(matches!(
                error,
                ExecutionError::Runtime(failure)
                    if matches!(failure.cause, RuntimeError::ArrayIndexOutOfBounds { .. })
            ));
        }
    }

    #[test]
    fn memchr_host_rejects_revoked_payloads_without_publishing_a_result() {
        use tidepool_heap::{
            execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind,
        };
        let descriptor = Arc::new(
            ObjectDescriptor::external(ExternalStorageKind::Bytes, &testing::target()).unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let machine = crate::machine_state::MachineState::new();
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
        machine.store_external_bytes(payload, 0, b"abcb").unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(payload);
        }
        machine
            .revoke_external_payload(payload, ExternalStorageKind::Bytes)
            .unwrap();
        let mut output = i64::MIN;
        let status = unsafe {
            prepared_text_memchr(
                &mut vmctx,
                reference,
                Arc::as_ptr(&descriptor),
                0,
                4,
                b'a' as i64,
                &mut output,
            )
        };
        assert_eq!(
            status,
            crate::prepared_control::CallStatus::IntegrityFailure as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
        assert_eq!(output, i64::MIN);
    }
}
