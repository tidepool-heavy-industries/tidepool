//! Prepared failure values: authenticated message storage and deliberate machine
//! disposition, independent of GHC's exception heap representation.

use crate::host_fns::RuntimeError;
use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use std::sync::Arc;
use tidepool_repr::execution_schema::WiredInErrorKind;

/// Decode only a complete NUL-terminated span owned by the compiled program.
/// This does not dereference the numeric address or force a Haskell value.
pub(super) fn wired_in_failure(
    bytes: &super::static_bytes::PinnedBytes,
    kind: WiredInErrorKind,
    address: usize,
) -> Result<RuntimeError, RuntimeError> {
    if kind == WiredInErrorKind::AbsentSumField {
        return Ok(RuntimeError::WiredInError {
            kind,
            message: "entered absent sum field!".into(),
        });
    }
    let length = bytes
        .c_string_len(address)
        .ok_or(RuntimeError::BadPointer)?;
    let raw = bytes
        .read_range(address, length)
        .ok_or(RuntimeError::BadPointer)?;
    let text = String::from_utf8_lossy(raw);
    let untangle = |message: &str| {
        let (location, details) = match text.split_once('|') {
            Some((location, details)) => (location, format!(" {details}")),
            None => (text.as_ref(), String::new()),
        };
        format!("{location}: {message}{details}\n")
    };
    use WiredInErrorKind::*;
    let message = match kind {
        PatternMatch => untangle("Non-exhaustive patterns in"),
        NonExhaustiveGuards => untangle("Non-exhaustive guards in"),
        RecordConstruction => untangle("Missing field in record construction"),
        NoMethodBinding => untangle("No instance nor default method for class operation"),
        RecordSelector => format!("No match in record selector {text}"),
        _ => text.into_owned(),
    };
    Ok(match kind {
        PatternMatch | NonExhaustiveGuards => RuntimeError::PatternMatchFailure(message),
        _ => RuntimeError::WiredInError { kind, message },
    })
}

fn decode_kind(tag: u64) -> Option<WiredInErrorKind> {
    use WiredInErrorKind::*;
    Some(match tag {
        0 => PatternMatch,
        1 => NonExhaustiveGuards,
        2 => RecordSelector,
        3 => RecordConstruction,
        4 => NoMethodBinding,
        5 => DeferredType,
        6 => Impossible,
        7 => ImpossibleConstraint,
        8 => Absent,
        9 => AbsentConstraint,
        10 => AbsentSumField,
        _ => return None,
    })
}

/// Record a GHC wired-in bottom without forcing or collecting. The byte-pool
/// pointer is a compiled-owner capability; the generated address is accepted
/// only when the pool authenticates its complete NUL-terminated message.
///
/// # Safety
/// `vmctx` belongs to the active generated call and a non-null `pool` is the
/// frozen owner retained by that call's compiled program.
pub(super) unsafe extern "C" fn prepared_wired_in_error(
    vmctx: *mut crate::context::VMContext,
    pool: *const super::static_bytes::PinnedBytes,
    kind_tag: u64,
    address: usize,
) -> i32 {
    use crate::prepared_control::CallStatus;

    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != CallStatus::Success {
        return status as i32;
    }
    let failure = match (decode_kind(kind_tag), unsafe { pool.as_ref() }) {
        (Some(kind), Some(bytes)) => wired_in_failure(bytes, kind, address),
        _ => Err(RuntimeError::BadPointer),
    };
    machine.set_first_cause(match failure {
        Ok(error) | Err(error) => error,
    });
    machine.prepared_call_status() as i32
}

pub(super) fn emit_wired_in_error(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    bytes: &Arc<super::static_bytes::PinnedBytes>,
    kind: WiredInErrorKind,
    address: Option<Value>,
) -> Result<(), super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    let host = pipeline
        .module
        .declare_function("prepared_wired_in_error", Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let pool = builder
        .ins()
        .iconst(types::I64, Arc::as_ptr(bytes) as usize as i64);
    let kind_tag = builder.ins().iconst(types::I64, kind as u8 as i64);
    let address = address.unwrap_or_else(|| builder.ins().iconst(types::I64, 0));
    let call = builder.ins().call(host, &[vmctx, pool, kind_tag, address]);
    let status = builder.inst_results(call)[0];
    super::no_success::emit_status(builder, status);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Arc};

    #[test]
    fn wired_message_uses_owned_utf8_and_ghc_untangle() {
        let payload: Arc<[u8]> = Arc::from(&b"Suite.hs:3|f\0"[..]);
        let address = payload.as_ptr() as usize;
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::from([(
            b"Suite.hs:3|f".to_vec(),
            payload,
        )]));
        assert_eq!(
            wired_in_failure(&pool, WiredInErrorKind::PatternMatch, address),
            Ok(RuntimeError::PatternMatchFailure(
                "Suite.hs:3: Non-exhaustive patterns in f\n".into()
            ))
        );
        assert_eq!(
            wired_in_failure(&pool, WiredInErrorKind::PatternMatch, 0),
            Err(RuntimeError::BadPointer)
        );
    }

    #[test]
    fn wired_message_is_bounded_and_lossily_owns_invalid_utf8() {
        let payload: Arc<[u8]> = Arc::from(&b"bad\xffmessage\0tail"[..]);
        let address = payload.as_ptr() as usize;
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::from([(
            b"bad\xffmessage\0tail".to_vec(),
            payload,
        )]));
        assert_eq!(
            wired_in_failure(&pool, WiredInErrorKind::DeferredType, address),
            Ok(RuntimeError::WiredInError {
                kind: WiredInErrorKind::DeferredType,
                message: "bad\u{fffd}message".into(),
            })
        );
        assert_eq!(
            wired_in_failure(&pool, WiredInErrorKind::DeferredType, address + 12),
            Err(RuntimeError::BadPointer)
        );
    }

    #[test]
    fn absent_sum_field_is_nullary_and_needs_no_message_address() {
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::new());
        assert_eq!(
            wired_in_failure(&pool, WiredInErrorKind::AbsentSumField, usize::MAX),
            Ok(RuntimeError::WiredInError {
                kind: WiredInErrorKind::AbsentSumField,
                message: "entered absent sum field!".into(),
            })
        );
    }

    #[test]
    fn host_preserves_first_cause_before_decoding_untrusted_arguments() {
        let machine = crate::machine_state::MachineState::new();
        machine.set_first_cause(RuntimeError::Cancelled);
        let mut vmctx = crate::context::VMContext::new(
            std::ptr::null_mut(),
            std::ptr::null(),
            crate::host_fns::gc_trigger,
        );
        vmctx.machine_state = &machine as *const _ as *mut _;
        let status =
            unsafe { prepared_wired_in_error(&mut vmctx, std::ptr::null(), u64::MAX, usize::MAX) };
        assert_eq!(
            status,
            crate::prepared_control::CallStatus::Cancelled as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
    }

    #[test]
    fn host_rejects_invalid_kind_and_null_pool_as_bad_pointer() {
        let pool = super::super::static_bytes::PinnedBytes::new(BTreeMap::new());
        for (pool, kind) in [
            (&pool as *const _, u64::MAX),
            (std::ptr::null(), WiredInErrorKind::AbsentSumField as u64),
        ] {
            let machine = crate::machine_state::MachineState::new();
            let mut vmctx = crate::context::VMContext::new(
                std::ptr::null_mut(),
                std::ptr::null(),
                crate::host_fns::gc_trigger,
            );
            vmctx.machine_state = &machine as *const _ as *mut _;
            let status = unsafe { prepared_wired_in_error(&mut vmctx, pool, kind, usize::MAX) };
            assert_eq!(
                status,
                crate::prepared_control::CallStatus::IntegrityFailure as i32
            );
            assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
        }
    }
}
