//! Exact registered Double-formatting intrinsics. Byte results use the
//! descriptor-backed external owner and are observed as owned snapshots.

use cranelift_codegen::ir::{
    self, condcodes::FloatCC, types, AbiParam, InstBuilder, MemFlags, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Linkage, Module};
use tidepool_heap::{
    execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind,
};
use tidepool_repr::execution_schema::{
    ForeignConvention, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

#[derive(Clone, Copy)]
pub(super) enum FormattingOperation {
    Bytes,
    PrecBytes,
    NeedsPrecedence,
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<FormattingOperation> {
    use RuntimeRep::*;
    let OperationIdentity::Intrinsic {
        symbol,
        convention: ForeignConvention::CCall,
    } = identity
    else {
        return None;
    };
    let (arguments, results, operation) = match symbol.as_str() {
        "prepared_render_double_bytes" => (
            vec![Float(64)],
            vec![UnliftedRef],
            FormattingOperation::Bytes,
        ),
        "prepared_render_double_prec_bytes" => (
            vec![Int(64), Float(64)],
            vec![UnliftedRef],
            FormattingOperation::PrecBytes,
        ),
        "prepared_double_needs_precedence" => (
            vec![Float(64)],
            vec![Int(64)],
            FormattingOperation::NeedsPrecedence,
        ),
        _ => return None,
    };
    (signature.arguments == arguments && signature.results == ResultContract::Returns(results))
        .then_some(operation)
}

/// GHC showSignedFloat includes negative zero but not negative NaN.
fn needs_precedence(value: f64) -> bool {
    value < 0.0 || (value == 0.0 && value.is_sign_negative())
}

fn render(value: f64, precedence: i64) -> String {
    let body = tidepool_bignum::haskell_show_double(value);
    if precedence > 6 && needs_precedence(value) {
        format!("({body})")
    } else {
        body
    }
}

unsafe fn render_into_wrapper(
    vmctx: *mut crate::context::VMContext,
    wrapper: *mut u8,
    value: f64,
    precedence: i64,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let text = render(value, precedence);
    let payload = match machine.allocate_external_storage(ExternalStorageKind::Bytes, text.len()) {
        Ok(payload) => payload,
        Err(_) => return super::arrays::array_error(machine, RuntimeError::HeapOverflow),
    };
    if machine
        .store_external_bytes(payload, 0, text.as_bytes())
        .is_err()
    {
        return super::arrays::array_error(machine, RuntimeError::BadPointer);
    }
    // The wrapper header and handle slot were reserved before this noncollecting call.
    unsafe { wrapper.add(8).cast::<*mut u8>().write(payload) };
    CallStatus::Success as i32
}

pub(super) unsafe extern "C" fn prepared_render_double_bytes(
    vmctx: *mut crate::context::VMContext,
    wrapper: *mut u8,
    value: f64,
) -> i32 {
    unsafe { render_into_wrapper(vmctx, wrapper, value, 0) }
}

pub(super) unsafe extern "C" fn prepared_render_double_prec_bytes(
    vmctx: *mut crate::context::VMContext,
    wrapper: *mut u8,
    precedence: i64,
    value: f64,
) -> i32 {
    unsafe { render_into_wrapper(vmctx, wrapper, value, precedence) }
}

pub(super) fn emit_needs_precedence(builder: &mut FunctionBuilder<'_>, value: Value) -> Vec<Value> {
    let zero = builder.ins().f64const(0.0);
    let negative = builder.ins().fcmp(FloatCC::LessThan, value, zero);
    let equal_zero = builder.ins().fcmp(FloatCC::Equal, value, zero);
    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), value);
    let sign = builder
        .ins()
        .icmp_imm(ir::condcodes::IntCC::SignedLessThan, bits, 0);
    let negative_zero = builder.ins().band(equal_zero, sign);
    let needs = builder.ins().bor(negative, negative_zero);
    vec![builder.ins().uextend(types::I64, needs)]
}

pub(super) fn emit_render_bytes(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    gc: FuncId,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
    precise: bool,
) -> Result<Vec<Value>, super::CompileError> {
    let gc = pipeline.module.declare_func_in_func(gc, builder.func);
    let object = crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, descriptor, gc);
    let header = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    builder.ins().store(MemFlags::trusted(), header, object, 0);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().store(MemFlags::trusted(), zero, object, 8);
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = if precise {
        vec![
            AbiParam::new(types::I64),
            AbiParam::new(types::I64),
            AbiParam::new(types::I64),
            AbiParam::new(types::F64),
        ]
    } else {
        vec![
            AbiParam::new(types::I64),
            AbiParam::new(types::I64),
            AbiParam::new(types::F64),
        ]
    };
    signature.returns = vec![AbiParam::new(types::I32)];
    let name = if precise {
        "prepared_render_double_prec_bytes"
    } else {
        "prepared_render_double_bytes"
    };
    let host = pipeline
        .module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let mut native_arguments = vec![vmctx, object];
    native_arguments.extend_from_slice(arguments);
    let call = builder.ins().call(host, &native_arguments);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    let result = builder.ins().bor_imm(object, 7);
    builder.declare_value_needs_stack_map(result);
    Ok(vec![result])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::*;

    fn run_intrinsic(symbol: &str, precedence: Option<i64>, value: f64) -> tidepool_bridge::Value {
        let mut wire = testing::wire_program();
        let predicate = symbol == "prepared_double_needs_precedence";
        let result_rep = if predicate {
            RuntimeRep::Int(64)
        } else {
            RuntimeRep::UnliftedRef
        };
        wire.signatures[0].results = ResultContract::Returns(vec![result_rep]);
        let mut arguments = Vec::new();
        let mut native_arguments = Vec::new();
        if let Some(precedence) = precedence {
            arguments.push(RuntimeRep::Int(64));
            native_arguments.push(Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: precedence.to_be_bytes().to_vec(),
            }));
        }
        arguments.push(RuntimeRep::Float(64));
        native_arguments.push(Atom::Scalar(ScalarLiteral::Float {
            bits: 64,
            bytes: value.to_bits().to_be_bytes().to_vec(),
        }));
        wire.signatures.push(Signature {
            arguments,
            results: ResultContract::Returns(vec![result_rep]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::Intrinsic {
                symbol: symbol.into(),
                convention: ForeignConvention::CCall,
            },
            signature: SignatureId(1),
        });
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: native_arguments,
            },
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(100)))]),
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(101),
                kind: CaseKind::MultiValue,
                scrutinee_results: ResultContract::Returns(vec![result_rep]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(100)],
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
        let program = super::super::CompiledProgram::compile(&linked).unwrap();
        program
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions {
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
            .values
            .into_iter()
            .next()
            .unwrap()
    }

    fn text_after_gc_program() -> super::super::CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Float(64)],
            results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::Intrinsic {
                symbol: "prepared_render_double_bytes".into(),
                convention: ForeignConvention::CCall,
            },
            signature: SignatureId(1),
        });
        wire.constructors = vec![ConstructorDecl {
            identity: testing::identity("Formatting", "Text"),
            family: testing::identity("Formatting", "TextType"),
            host_id: tidepool_repr::DataConId(1901),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::UnliftedRef],
            strict_fields: vec![false],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::UnliftedRef,
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![true],
            },
        }];
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("Formatting", "Garbage"),
            family: testing::identity("Formatting", "GarbageType"),
            host_id: tidepool_repr::DataConId(1902),
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
        let mut nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![Atom::Scalar(ScalarLiteral::Float {
                    bits: 64,
                    bytes: (-0.0_f64).to_bits().to_be_bytes().to_vec(),
                })],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![Atom::Ref(ValueRef::Local(ValueId(100)))],
            },
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(101)))]),
            ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(101),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 2,
                }],
            },
        ];
        let mut after_format = 3;
        for index in 0..24 {
            let construct = nodes.len();
            nodes.push(ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            });
            let case = nodes.len();
            nodes.push(ExprFrame::Case {
                scrutinee: construct,
                binder: ValueId(200 + index),
                kind: CaseKind::Polymorphic,
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: after_format,
                }],
            });
            after_format = case;
        }
        let format_case = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(102),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![ValueId(100)],
                body: after_format,
            }],
        });
        wire.expressions.nodes = nodes;
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
                *body = format_case;
            }
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        super::super::CompiledProgram::compile(&linked).unwrap()
    }
    #[test]
    fn signed_zero_nan_and_subnormal_follow_ghc_precedence() {
        assert_eq!(render(-0.0, 6), "-0.0");
        assert_eq!(render(-0.0, 7), "(-0.0)");
        assert_eq!(render(f64::NEG_INFINITY, 7), "(-Infinity)");
        assert_eq!(render(-f64::NAN, 7), "NaN");
        assert!(!needs_precedence(-f64::NAN));
        assert!(!needs_precedence(1.5));
        assert_eq!(render(f64::from_bits(1), 6), "5.0e-324");
    }

    #[test]
    fn prepared_formatter_intrinsics_return_owned_bytes_after_gc() {
        for (symbol, precedence, value, expected) in [
            ("prepared_render_double_bytes", None, -0.0, "-0.0"),
            (
                "prepared_render_double_bytes",
                None,
                1e23,
                "9.999999999999999e22",
            ),
            (
                "prepared_render_double_prec_bytes",
                Some(7),
                -1e23,
                "(-9.999999999999999e22)",
            ),
            ("prepared_render_double_prec_bytes", Some(6), -0.0, "-0.0"),
            ("prepared_render_double_prec_bytes", Some(7), -0.0, "(-0.0)"),
            ("prepared_render_double_bytes", None, -f64::NAN, "NaN"),
            (
                "prepared_render_double_bytes",
                None,
                f64::from_bits(1),
                "5.0e-324",
            ),
        ] {
            assert!(matches!(run_intrinsic(symbol, precedence, value),
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitByteArray(ref bytes))
                    if bytes == expected.as_bytes()));
        }
        assert!(matches!(
            run_intrinsic("prepared_double_needs_precedence", None, -0.0),
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(1))
        ));
        assert!(matches!(
            run_intrinsic("prepared_double_needs_precedence", None, -f64::NAN),
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(0))
        ));
    }

    #[test]
    fn formatted_bytes_survive_moving_gc_before_text_constructor() {
        let result = text_after_gc_program()
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions {
                    nursery_bytes: 128,
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(result.collections >= 2);
        assert!(matches!(result.values.as_slice(),
            [tidepool_bridge::Value::Con(id, fields)]
                if *id == tidepool_repr::DataConId(1901)
                    && matches!(fields.as_slice(),
                        [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitByteArray(bytes))]
                            if bytes == b"-0.0")));
    }
}
