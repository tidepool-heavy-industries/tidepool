//! Exact native-word operations: unsigned wide results remain two Word64
//! values, never an admitted 128-bit schema representation. `timesInt2#`
//! returns `(isHighNeeded, high, low)` as three Int64 values. Other scalar
//! results are high/low for add/multiply and quotient/remainder for division.
//! Division is noncollecting; its host wrapper must record failure before
//! publishing either result slot.

use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
use cranelift_codegen::ir::{self, types, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_repr::execution_schema::{OperationIdentity, ResultContract, RuntimeRep, Signature};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WideWordOperation {
    Plus2,
    Times2,
    TimesInt2,
    QuotRem2,
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<WideWordOperation> {
    use RuntimeRep::{Int, Word};
    let OperationIdentity::PrimOp(name) = identity else {
        return None;
    };
    let operation = match name.as_str() {
        "plusWord2#" if signature.arguments == [Word(64), Word(64)] => WideWordOperation::Plus2,
        "timesWord2#" if signature.arguments == [Word(64), Word(64)] => WideWordOperation::Times2,
        "timesInt2#" if signature.arguments == [Int(64), Int(64)] => {
            return (signature.results == ResultContract::Returns(vec![Int(64), Int(64), Int(64)]))
                .then_some(WideWordOperation::TimesInt2);
        }
        "quotRemWord2#" if signature.arguments == [Word(64), Word(64), Word(64)] => {
            WideWordOperation::QuotRem2
        }
        _ => return None,
    };
    (signature.results == ResultContract::Returns(vec![Word(64), Word(64)])).then_some(operation)
}

/// GHC requires high < divisor. Reject undefined inputs without a native trap
/// or truncated quotient; all arithmetic here is internal Rust, not the ABI.
pub(super) fn checked_quot_rem(
    high: u64,
    low: u64,
    divisor: u64,
) -> Result<(u64, u64), RuntimeError> {
    if divisor == 0 {
        return Err(RuntimeError::DivisionByZero);
    }
    if high >= divisor {
        return Err(RuntimeError::Overflow);
    }
    let numerator = (u128::from(high) << 64) | u128::from(low);
    Ok((
        (numerator / u128::from(divisor)) as u64,
        (numerator % u128::from(divisor)) as u64,
    ))
}

/// The caller owns two writable output slots; neither is written on error.
/// No collection or managed reference access occurs in this host call.
///
/// # Safety
/// `vmctx` belongs to the running prepared invocation and `output` points to
/// two writable `u64` words in its native caller frame.
pub(super) unsafe extern "C" fn prepared_quot_rem_word2(
    vmctx: *mut crate::context::VMContext,
    high: u64,
    low: u64,
    divisor: u64,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = if output.is_null() {
        Err(RuntimeError::BadPointer)
    } else {
        checked_quot_rem(high, low, divisor)
    };
    match result {
        Ok((quotient, remainder)) => {
            unsafe {
                output.write(quotient);
                output.add(1).write(remainder);
            }
            CallStatus::Success as i32
        }
        Err(cause) => {
            machine.set_first_cause(cause);
            machine.prepared_call_status() as i32
        }
    }
}

pub(super) fn emit(
    operation: WideWordOperation,
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let high_low = match operation {
        WideWordOperation::Plus2 => {
            let low = builder.ins().iadd(arguments[0], arguments[1]);
            let carry =
                builder
                    .ins()
                    .icmp(ir::condcodes::IntCC::UnsignedLessThan, low, arguments[0]);
            let high = builder.ins().uextend(types::I64, carry);
            vec![high, low]
        }
        WideWordOperation::Times2 => {
            let high = builder.ins().umulhi(arguments[0], arguments[1]);
            let low = builder.ins().imul(arguments[0], arguments[1]);
            vec![high, low]
        }
        WideWordOperation::TimesInt2 => {
            let high = builder.ins().smulhi(arguments[0], arguments[1]);
            let low = builder.ins().imul(arguments[0], arguments[1]);
            let low_sign = builder.ins().sshr_imm(low, 63);
            let high_needed = builder
                .ins()
                .icmp(ir::condcodes::IntCC::NotEqual, high, low_sign);
            let high_needed = builder.ins().uextend(types::I64, high_needed);
            vec![high_needed, high, low]
        }
        WideWordOperation::QuotRem2 => {
            let host =
                super::arrays::declare_host(builder, pipeline, "prepared_quot_rem_word2", 5)?;
            let slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
                ir::StackSlotKind::ExplicitSlot,
                16,
                3,
            ));
            let output = builder.ins().stack_addr(types::I64, slot, 0);
            let call = builder.ins().call(
                host,
                &[vmctx, arguments[0], arguments[1], arguments[2], output],
            );
            let status = builder.inst_results(call)[0];
            super::arrays::finish_checked_call(builder, status);
            vec![
                builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), output, 0),
                builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), output, 8),
            ]
        }
    };
    Ok(high_low)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::{testing, *};

    fn word(value: u64) -> Atom {
        Atom::Scalar(ScalarLiteral::Word {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    }

    fn int(value: i64) -> Atom {
        Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    }

    fn run_operation(
        name: &str,
        arguments: Vec<RuntimeRep>,
        results: Vec<RuntimeRep>,
        atoms: Vec<Atom>,
    ) -> Result<Vec<tidepool_bridge::Value>, super::super::ExecutionError> {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(results.clone());
        wire.signatures.push(Signature {
            arguments,
            results: ResultContract::Returns(results),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments: atoms,
        };
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        let program = super::super::CompiledProgram::compile(&linked).unwrap();
        program
            .run_entry(
                ValueId(0),
                &[],
                &Default::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .map(|run| run.values)
    }

    fn pair(values: Vec<tidepool_bridge::Value>) -> (u64, u64) {
        use tidepool_bridge::Value;
        use tidepool_repr::Literal;
        let [Value::Lit(Literal::LitWord(high)), Value::Lit(Literal::LitWord(low))] =
            values.as_slice()
        else {
            panic!("expected two Word64 results: {values:?}");
        };
        (*high, *low)
    }

    fn int_triple(values: Vec<tidepool_bridge::Value>) -> (i64, i64, i64) {
        use tidepool_bridge::Value;
        use tidepool_repr::Literal;
        let [Value::Lit(Literal::LitInt(high_needed)), Value::Lit(Literal::LitInt(high)), Value::Lit(Literal::LitInt(low))] =
            values.as_slice()
        else {
            panic!("expected three Int64 results: {values:?}");
        };
        (*high_needed, *high, *low)
    }

    #[test]
    fn wide_word_exact_signatures() {
        let w = RuntimeRep::Word(64);
        let sig = |arguments, results| Signature {
            arguments,
            results: ResultContract::Returns(results),
        };
        for (name, arguments, operation) in [
            ("plusWord2#", vec![w, w], WideWordOperation::Plus2),
            ("timesWord2#", vec![w, w], WideWordOperation::Times2),
            ("quotRemWord2#", vec![w, w, w], WideWordOperation::QuotRem2),
        ] {
            let identity = OperationIdentity::PrimOp(name.into());
            assert_eq!(
                recognize(&identity, &sig(arguments.clone(), vec![w, w])),
                Some(operation)
            );
            assert_eq!(recognize(&identity, &sig(arguments.clone(), vec![w])), None);
            assert_eq!(
                recognize(
                    &identity,
                    &sig(arguments[..arguments.len() - 1].to_vec(), vec![w, w])
                ),
                None
            );
            assert_eq!(
                recognize(
                    &OperationIdentity::PrimOp(format!("{name}wrong")),
                    &sig(arguments.clone(), vec![w, w])
                ),
                None
            );
            assert_eq!(
                recognize(
                    &identity,
                    &Signature {
                        arguments,
                        results: ResultContract::NoSuccess,
                    }
                ),
                None
            );
        }
    }

    #[test]
    fn times_int2_requires_exact_identity_and_signature() {
        let i = RuntimeRep::Int(64);
        let exact = Signature {
            arguments: vec![i, i],
            results: ResultContract::Returns(vec![i, i, i]),
        };
        assert_eq!(
            recognize(&OperationIdentity::PrimOp("timesInt2#".into()), &exact),
            Some(WideWordOperation::TimesInt2)
        );
        for signature in [
            Signature {
                arguments: vec![i],
                ..exact.clone()
            },
            Signature {
                results: ResultContract::Returns(vec![i, i]),
                ..exact.clone()
            },
            Signature {
                results: ResultContract::NoSuccess,
                ..exact.clone()
            },
        ] {
            assert_eq!(
                recognize(&OperationIdentity::PrimOp("timesInt2#".into()), &signature),
                None
            );
        }
        assert_eq!(
            recognize(&OperationIdentity::PrimOp("timesInt2".into()), &exact),
            None
        );
    }

    #[test]
    fn times_int2_real_adapter_uses_ghc_result_order() {
        let i = RuntimeRep::Int(64);
        for (left, right) in [
            (-7_i64, 9_i64),
            (i64::MAX, 2),
            (i64::MIN, -1),
            (i64::MIN, i64::MIN),
        ] {
            let product = i128::from(left) * i128::from(right);
            let low = product as i64;
            let high = (product >> 64) as i64;
            let high_needed = i64::from(high != (low >> 63));
            assert_eq!(
                int_triple(
                    run_operation(
                        "timesInt2#",
                        vec![i, i],
                        vec![i, i, i],
                        vec![int(left), int(right)]
                    )
                    .unwrap()
                ),
                (high_needed, high, low),
                "{left} * {right}"
            );
        }
    }

    #[test]
    fn char_and_clz_exact_signatures() {
        use super::super::primitives::{BasicScalarFamily, BasicScalarOperation, ScalarFamily};
        let w = RuntimeRep::Word(64);
        let i = RuntimeRep::Int(64);
        let sig = |arguments, results| Signature {
            arguments,
            results: ResultContract::Returns(results),
        };
        for (name, arguments, results, operation) in [
            ("ord#", vec![w], vec![i], BasicScalarOperation::Ord),
            ("geChar#", vec![w, w], vec![i], BasicScalarOperation::GeChar),
            ("ltChar#", vec![w, w], vec![i], BasicScalarOperation::LtChar),
            ("clz#", vec![w], vec![w], BasicScalarOperation::Clz),
        ] {
            let identity = OperationIdentity::PrimOp(name.into());
            assert_eq!(
                BasicScalarFamily::recognize(&identity, &sig(arguments.clone(), results.clone())),
                Some(operation)
            );
            assert_eq!(
                BasicScalarFamily::recognize(&identity, &sig(arguments.clone(), vec![])),
                None
            );
            assert_eq!(
                BasicScalarFamily::recognize(&identity, &sig(vec![], results)),
                None
            );
        }
    }

    #[test]
    fn wide_word_real_adapter_high_low_and_quotient_remainder() {
        let w = RuntimeRep::Word(64);
        assert_eq!(
            pair(
                run_operation(
                    "plusWord2#",
                    vec![w, w],
                    vec![w, w],
                    vec![word(u64::MAX), word(1)]
                )
                .unwrap()
            ),
            (1, 0)
        );
        assert_eq!(
            pair(
                run_operation("plusWord2#", vec![w, w], vec![w, w], vec![word(7), word(9)])
                    .unwrap()
            ),
            (0, 16)
        );
        assert_eq!(
            pair(
                run_operation(
                    "timesWord2#",
                    vec![w, w],
                    vec![w, w],
                    vec![word(u64::MAX), word(2)]
                )
                .unwrap()
            ),
            (1, u64::MAX - 1)
        );
        assert_eq!(
            pair(
                run_operation(
                    "quotRemWord2#",
                    vec![w, w, w],
                    vec![w, w],
                    vec![word(1), word(3), word(2)]
                )
                .unwrap()
            ),
            (1 << 63 | 1, 1)
        );
    }

    #[test]
    fn word_clz_and_char_real_adapter_results() {
        use tidepool_bridge::Value;
        use tidepool_repr::Literal;
        let w = RuntimeRep::Word(64);
        let i = RuntimeRep::Int(64);
        assert!(matches!(
            run_operation("clz#", vec![w], vec![w], vec![word(0)])
                .unwrap()
                .as_slice(),
            [Value::Lit(Literal::LitWord(64))]
        ));
        assert!(matches!(
            run_operation("clz#", vec![w], vec![w], vec![word(1 << 63)])
                .unwrap()
                .as_slice(),
            [Value::Lit(Literal::LitWord(0))]
        ));
        assert!(matches!(
            run_operation("ord#", vec![w], vec![i], vec![word(0x10ffff)])
                .unwrap()
                .as_slice(),
            [Value::Lit(Literal::LitInt(0x10ffff))]
        ));
        for (left, right, expected) in [
            (0x10ffff, 0x10ffff, 1),
            (0, 0x10ffff, 0),
            (0x10ffff, 0, 1),
            (u64::MAX, 0, 1),
        ] {
            let values = run_operation(
                "geChar#",
                vec![w, w],
                vec![i],
                vec![word(left), word(right)],
            )
            .unwrap();
            assert!(matches!(
                values.as_slice(),
                [Value::Lit(Literal::LitInt(actual))] if *actual == expected
            ));
        }
        // `ltChar#` is the strict complement of `geChar#` over the same
        // unsigned code-point comparison.
        for (left, right, expected) in [
            (0x10ffff, 0x10ffff, 0),
            (0, 0x10ffff, 1),
            (0x10ffff, 0, 0),
            (0, u64::MAX, 1),
        ] {
            let values = run_operation(
                "ltChar#",
                vec![w, w],
                vec![i],
                vec![word(left), word(right)],
            )
            .unwrap();
            assert!(matches!(
                values.as_slice(),
                [Value::Lit(Literal::LitInt(actual))] if *actual == expected
            ));
        }
    }

    #[test]
    fn wide_word_division_real_adapter_failures_are_reusable() {
        let w = RuntimeRep::Word(64);
        for (high, divisor, cause) in [
            (0, 0, RuntimeError::DivisionByZero),
            (2, 2, RuntimeError::Overflow),
        ] {
            let error = run_operation(
                "quotRemWord2#",
                vec![w, w, w],
                vec![w, w],
                vec![word(high), word(1), word(divisor)],
            )
            .unwrap_err();
            assert!(matches!(
                error,
                super::super::ExecutionError::Runtime(failure)
                    if failure.cause == cause
                        && failure.disposition == crate::machine_state::MachineDisposition::Reusable
            ));
        }
    }

    #[test]
    fn wide_word_host_does_not_publish_either_slot_on_failure() {
        let machine = crate::machine_state::MachineState::new();
        let mut buffer = [0_u64; 2];
        let mut vmctx = crate::context::VMContext::new(
            buffer.as_mut_ptr().cast(),
            buffer.as_mut_ptr().wrapping_add(buffer.len()).cast(),
            crate::host_fns::gc_trigger,
        );
        vmctx.machine_state = &machine as *const _ as *mut _;
        let mut output = [0xaaaa_u64, 0xbbbb];
        let status = unsafe { prepared_quot_rem_word2(&mut vmctx, 1, 0, 1, output.as_mut_ptr()) };
        assert_eq!(status, CallStatus::LanguageFailure as i32);
        assert_eq!(output, [0xaaaa, 0xbbbb]);
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Overflow));
    }

    #[test]
    fn wide_word_division_domain_and_result_order() {
        assert_eq!(checked_quot_rem(1, 3, 2), Ok((1 << 63 | 1, 1)));
        assert_eq!(checked_quot_rem(0, 42, 5), Ok((8, 2)));
        assert_eq!(checked_quot_rem(0, 1, 0), Err(RuntimeError::DivisionByZero));
        assert_eq!(checked_quot_rem(2, 0, 2), Err(RuntimeError::Overflow));
    }
}
