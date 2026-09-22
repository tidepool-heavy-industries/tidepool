//! Exact registered `parseISO8601` intrinsic over authenticated `Text` bytes.

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Linkage, Module};
use tidepool_heap::{
    execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind,
};
use tidepool_repr::execution_schema::{
    ForeignConvention, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

use crate::{host_fns::RuntimeError, prepared_control::CallStatus};

pub(super) const PARSE_ISO8601_HOST: &str = "prepared_parse_iso8601";

pub(super) fn recognize(identity: &OperationIdentity, signature: &Signature) -> bool {
    matches!(identity, OperationIdentity::Intrinsic {
        symbol,
        convention: ForeignConvention::CCall,
    } if symbol == PARSE_ISO8601_HOST)
        && signature.arguments
            == [
                RuntimeRep::UnliftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
            ]
        && signature.results
            == ResultContract::Returns(vec![
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
                RuntimeRep::UnliftedRef,
            ])
}

fn checked_span(value: i64, len: usize) -> Result<usize, RuntimeError> {
    usize::try_from(value).map_err(|_| RuntimeError::ArrayIndexOutOfBounds { index: value, len })
}

/// Parse one authenticated UTF-8 span. Typed parse failure is ordinary output;
/// malformed managed storage remains a runtime integrity failure.
pub(super) unsafe extern "C" fn prepared_parse_iso8601(
    vmctx: *mut crate::context::VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    offset: i64,
    length: i64,
    error_wrapper: *mut u8,
    success_output: *mut i64,
    millis_output: *mut i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if error_wrapper.is_null() || success_output.is_null() || millis_output.is_null() {
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
        let offset = checked_span(offset, len)?;
        let length = checked_span(length, len)?;
        let bytes = machine
            .read_external_payload_offset(published, offset, length)
            .map_err(super::byte_arrays::byte_range_error)?;
        let input = std::str::from_utf8(&bytes).map_err(|_| RuntimeError::BadPointer)?;
        let parsed = chrono::DateTime::parse_from_rfc3339(input.trim());
        let (success, millis, message) = match parsed {
            Ok(value) => (1, value.timestamp_millis(), String::new()),
            Err(error) => (0, 0, format!("parseISO8601: {input:?}: {error}")),
        };
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, message.len())
            .map_err(|_| RuntimeError::HeapOverflow)?;
        machine
            .store_external_bytes(payload, 0, message.as_bytes())
            .map_err(|_| RuntimeError::BadPointer)?;
        unsafe {
            error_wrapper.add(8).cast::<*mut u8>().write(payload);
            success_output.write(success);
            millis_output.write(millis);
        }
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) fn emit_parse_iso8601(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: FuncId,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let gc = pipeline.module.declare_func_in_func(gc, builder.func);
    let error_wrapper = crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, descriptor, gc);
    let header = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    builder
        .ins()
        .store(MemFlags::trusted(), header, error_wrapper, 0);
    let zero = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .store(MemFlags::trusted(), zero, error_wrapper, 8);

    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 8];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function(PARSE_ISO8601_HOST, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let success_output = super::arrays::output_slot(builder);
    let millis_output = super::arrays::output_slot(builder);
    let call = builder.ins().call(
        host,
        &[
            vmctx,
            arguments[0],
            owner,
            arguments[1],
            arguments[2],
            error_wrapper,
            success_output,
            millis_output,
        ],
    );
    super::arrays::finish_checked_call(builder, builder.inst_results(call)[0]);
    let error_bytes = builder.ins().bor_imm(error_wrapper, 7);
    builder.declare_value_needs_stack_map(error_bytes);
    Ok(vec![
        builder
            .ins()
            .load(types::I64, MemFlags::trusted(), success_output, 0),
        builder
            .ins()
            .load(types::I64, MemFlags::trusted(), millis_output, 0),
        error_bytes,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature() -> Signature {
        Signature {
            arguments: vec![
                RuntimeRep::UnliftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
            ],
            results: ResultContract::Returns(vec![
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
                RuntimeRep::UnliftedRef,
            ]),
        }
    }

    #[test]
    fn time_intrinsic_requires_exact_identity_and_signature() {
        let identity = OperationIdentity::Intrinsic {
            symbol: PARSE_ISO8601_HOST.into(),
            convention: ForeignConvention::CCall,
        };
        assert!(recognize(&identity, &signature()));
        let mut wrong_argument = signature();
        wrong_argument.arguments[1] = RuntimeRep::Word(64);
        assert!(!recognize(&identity, &wrong_argument));
        let mut wrong_result = signature();
        wrong_result.results = ResultContract::Returns(vec![RuntimeRep::Int(64)]);
        assert!(!recognize(&identity, &wrong_result));
        assert!(!recognize(
            &OperationIdentity::PrimOp(PARSE_ISO8601_HOST.into()),
            &signature()
        ));
    }
}
