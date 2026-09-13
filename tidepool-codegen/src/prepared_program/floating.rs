//! Pure floating operations retain GHC identities and exact representations.

use super::primitives::ScalarFamily;
use cranelift_codegen::ir::{condcodes::FloatCC, types, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_repr::execution_schema::{
    ForeignConvention, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

fn returns_exact(signature: &Signature, expected: &[RuntimeRep]) -> bool {
    match &signature.results {
        ResultContract::Returns(reps) => reps == expected,
        ResultContract::NoSuccess => false,
    }
}

pub(super) struct FloatingFamily;

#[derive(Clone, Copy)]
pub(super) enum FloatingOperation {
    NearestDouble,
    Negate,
    Binary(BinaryKind),
    Compare(CompareKind),
    Convert { from_float: bool },
}

#[derive(Clone, Copy)]
pub(super) enum BinaryKind {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy)]
pub(super) enum CompareKind {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl ScalarFamily for FloatingFamily {
    type Operation = FloatingOperation;

    /// This C symbol has no Haskell unfolding: ghc-internal implements it in
    /// C as ties-even rounding. Cranelift nearest implements that operation,
    /// preserving signed zero and IEEE exceptional values without a host call.
    /// A similarly named primop or a different signature has no such authority.
    fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<Self::Operation> {
        let OperationIdentity::Intrinsic {
            symbol,
            convention: ForeignConvention::CCall,
        } = identity
        else {
            let OperationIdentity::PrimOp(name) = identity else {
                return None;
            };
            let operation = match name.as_str() {
                "negateDouble#" | "negateFloat#" => FloatingOperation::Negate,
                "+##" | "-##" | "*##" | "/##" => FloatingOperation::Binary(match name.as_str() {
                    "+##" => BinaryKind::Add,
                    "-##" => BinaryKind::Sub,
                    "*##" => BinaryKind::Mul,
                    _ => BinaryKind::Div,
                }),
                "plusFloat#" | "minusFloat#" | "timesFloat#" | "divideFloat#" => {
                    FloatingOperation::Binary(match name.as_str() {
                        "plusFloat#" => BinaryKind::Add,
                        "minusFloat#" => BinaryKind::Sub,
                        "timesFloat#" => BinaryKind::Mul,
                        _ => BinaryKind::Div,
                    })
                }
                "==##" | "/=##" | "<##" | "<=##" | ">##" | ">=##" => {
                    FloatingOperation::Compare(match name.as_str() {
                        "==##" => CompareKind::Eq,
                        "/=##" => CompareKind::Ne,
                        "<##" => CompareKind::Lt,
                        "<=##" => CompareKind::Le,
                        ">##" => CompareKind::Gt,
                        _ => CompareKind::Ge,
                    })
                }
                "eqFloat#" | "neFloat#" | "ltFloat#" | "leFloat#" | "gtFloat#" | "geFloat#" => {
                    FloatingOperation::Compare(match name.as_str() {
                        "eqFloat#" => CompareKind::Eq,
                        "neFloat#" => CompareKind::Ne,
                        "ltFloat#" => CompareKind::Lt,
                        "leFloat#" => CompareKind::Le,
                        "gtFloat#" => CompareKind::Gt,
                        _ => CompareKind::Ge,
                    })
                }
                "float2Double#" => FloatingOperation::Convert { from_float: true },
                "double2Float#" => FloatingOperation::Convert { from_float: false },
                _ => return None,
            };
            let width = if name.contains("Float") || name.contains("float") {
                32
            } else {
                64
            };
            let valid = match operation {
                FloatingOperation::Negate => {
                    signature.arguments == [RuntimeRep::Float(width)]
                        && returns_exact(signature, &[RuntimeRep::Float(width)])
                }
                FloatingOperation::Compare(_) => {
                    signature.arguments == [RuntimeRep::Float(width), RuntimeRep::Float(width)]
                        && returns_exact(signature, &[RuntimeRep::Int(64)])
                }
                FloatingOperation::Convert { from_float } => {
                    signature.arguments == [RuntimeRep::Float(if from_float { 32 } else { 64 })]
                        && returns_exact(
                            signature,
                            &[RuntimeRep::Float(if from_float { 64 } else { 32 })],
                        )
                }
                _ => {
                    signature.arguments == [RuntimeRep::Float(width); 2]
                        && returns_exact(signature, &[RuntimeRep::Float(width)])
                }
            };
            return valid.then_some(operation);
        };
        match symbol.as_str() {
            "rintDouble"
                if (signature.arguments == [RuntimeRep::Float(64)]
                    || signature.arguments == [RuntimeRep::Float(64), RuntimeRep::Void])
                    && returns_exact(signature, &[RuntimeRep::Float(64)]) =>
            {
                Some(FloatingOperation::NearestDouble)
            }
            _ => None,
        }
    }

    fn emit(
        operation: Self::Operation,
        builder: &mut FunctionBuilder<'_>,
        arguments: &[Value],
    ) -> Vec<Value> {
        match operation {
            FloatingOperation::NearestDouble => vec![builder.ins().nearest(arguments[0])],
            FloatingOperation::Negate => vec![builder.ins().fneg(arguments[0])],
            FloatingOperation::Binary(kind) => {
                let value = match kind {
                    BinaryKind::Add => builder.ins().fadd(arguments[0], arguments[1]),
                    BinaryKind::Sub => builder.ins().fsub(arguments[0], arguments[1]),
                    BinaryKind::Mul => builder.ins().fmul(arguments[0], arguments[1]),
                    BinaryKind::Div => builder.ins().fdiv(arguments[0], arguments[1]),
                };
                vec![value]
            }
            FloatingOperation::Compare(kind) => {
                let cc = match kind {
                    CompareKind::Eq => FloatCC::Equal,
                    CompareKind::Ne => FloatCC::NotEqual,
                    CompareKind::Lt => FloatCC::LessThan,
                    CompareKind::Le => FloatCC::LessThanOrEqual,
                    CompareKind::Gt => FloatCC::GreaterThan,
                    CompareKind::Ge => FloatCC::GreaterThanOrEqual,
                };
                let cmp = builder.ins().fcmp(cc, arguments[0], arguments[1]);
                vec![builder.ins().uextend(types::I64, cmp)]
            }
            FloatingOperation::Convert { from_float } => {
                let value = if from_float {
                    builder.ins().fpromote(types::F64, arguments[0])
                } else {
                    builder.ins().fdemote(types::F32, arguments[0])
                };
                vec![value]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::{testing, *};

    fn run(
        identity: &str,
        arguments: Vec<RuntimeRep>,
        results: Vec<RuntimeRep>,
        atoms: Vec<Atom>,
    ) -> Vec<tidepool_bridge::Value> {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(results.clone());
        wire.signatures.push(Signature {
            arguments,
            results: ResultContract::Returns(results),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp(identity.into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments: atoms,
        };
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        super::super::CompiledProgram::compile(&linked)
            .unwrap()
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
            .values
    }

    fn float(bits: u8, value: u64) -> Atom {
        let bytes = if bits == 32 {
            (value as u32).to_be_bytes().to_vec()
        } else {
            value.to_be_bytes().to_vec()
        };
        Atom::Scalar(ScalarLiteral::Float { bits, bytes })
    }

    #[test]
    fn w5_a3_rint_double_exact_identity_rounds_ties_even() {
        // GHC's foreign call carries a final State# token even for this pure
        // intrinsic. Preserve its logical position; only physical ABI erases it.
        for state_token in [false, true] {
            for (input, expected) in [(0.5_f64, 0.0_f64), (1.5, 2.0), (-0.5, -0.0), (-1.5, -2.0)] {
                let mut wire = testing::wire_program();
                wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Float(64)]);
                wire.signatures.push(Signature {
                    arguments: vec![RuntimeRep::Float(64)],
                    results: ResultContract::Returns(vec![RuntimeRep::Float(64)]),
                });
                if state_token {
                    wire.signatures[1].arguments.push(RuntimeRep::Void);
                }
                wire.operations.push(OperationDecl {
                    identity: OperationIdentity::Intrinsic {
                        symbol: "rintDouble".into(),
                        convention: ForeignConvention::CCall,
                    },
                    signature: SignatureId(1),
                });
                wire.expressions.nodes[0] = ExprFrame::Operation {
                    operation: OperationId(0),
                    arguments: vec![Atom::Scalar(ScalarLiteral::Float {
                        bits: 64,
                        bytes: input.to_bits().to_be_bytes().to_vec(),
                    })],
                };
                if state_token {
                    let ExprFrame::Operation { arguments, .. } = &mut wire.expressions.nodes[0]
                    else {
                        unreachable!()
                    };
                    arguments.push(Atom::Void);
                }
                let linked =
                    link_program(testing::prepare(wire).unwrap(), &MachineImports::default())
                        .unwrap();
                let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
                let result = compiled
                    .run_entry(
                        ValueId(0),
                        &[],
                        &super::super::RunOptions::default(),
                        Arc::new(AtomicBool::new(false)),
                    )
                    .unwrap();
                assert!(matches!(result.values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                if *bits == expected.to_bits()));
            }
        }
    }

    #[test]
    fn ghc_float_families_preserve_ieee_values_and_convert() {
        let values = run(
            "plusFloat#",
            vec![RuntimeRep::Float(32), RuntimeRep::Float(32)],
            vec![RuntimeRep::Float(32)],
            vec![
                float(32, f32::from_bits(0x8000_0000).to_bits() as u64),
                float(32, 0),
            ],
        );
        assert!(
            matches!(values.as_slice(), [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitFloat(bits))] if *bits == 0)
        );
        let values = run(
            "eqFloat#",
            vec![RuntimeRep::Float(32), RuntimeRep::Float(32)],
            vec![RuntimeRep::Int(64)],
            vec![float(32, 0x7fc0_0001), float(32, 0x7fc0_0001)],
        );
        assert!(
            matches!(values.as_slice(), [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(value))] if *value == 0)
        );
        let values = run(
            "float2Double#",
            vec![RuntimeRep::Float(32)],
            vec![RuntimeRep::Float(64)],
            vec![float(32, 0x8000_0000)],
        );
        assert!(
            matches!(values.as_slice(), [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))] if *bits == (-0.0f64).to_bits())
        );
    }

    #[test]
    fn ghc_negate_uses_native_fneg_and_preserves_signed_zero() {
        for (input, expected) in [(0.0_f64, -0.0_f64), (-0.0, 0.0), (1.5, -1.5)] {
            let values = run(
                "negateDouble#",
                vec![RuntimeRep::Float(64)],
                vec![RuntimeRep::Float(64)],
                vec![float(64, input.to_bits())],
            );
            assert!(matches!(values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                    if *bits == expected.to_bits()));
        }
        for (input, expected) in [(0.0_f32, -0.0_f32), (-0.0, 0.0), (1.5, -1.5)] {
            let values = run(
                "negateFloat#",
                vec![RuntimeRep::Float(32)],
                vec![RuntimeRep::Float(32)],
                vec![float(32, u64::from(input.to_bits()))],
            );
            assert!(matches!(values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitFloat(bits))]
                    if *bits == u64::from(expected.to_bits())));
        }
    }

    #[test]
    fn ghc_float_family_rejects_wrong_signatures() {
        assert!(FloatingFamily::recognize(
            &OperationIdentity::PrimOp("plusFloat#".into()),
            &Signature {
                arguments: vec![RuntimeRep::Float(64), RuntimeRep::Float(64)],
                results: ResultContract::Returns(vec![RuntimeRep::Float(64)])
            }
        )
        .is_none());
        assert!(FloatingFamily::recognize(
            &OperationIdentity::PrimOp("eqFloat#".into()),
            &Signature {
                arguments: vec![RuntimeRep::Float(32), RuntimeRep::Float(32)],
                results: ResultContract::Returns(vec![RuntimeRep::Float(32)])
            }
        )
        .is_none());
        for (name, arguments, results) in [
            (
                "negateDouble#",
                vec![RuntimeRep::Float(32)],
                vec![RuntimeRep::Float(32)],
            ),
            (
                "negateDouble#",
                vec![RuntimeRep::Float(64); 2],
                vec![RuntimeRep::Float(64)],
            ),
            (
                "negateFloat#",
                vec![RuntimeRep::Float(64)],
                vec![RuntimeRep::Float(64)],
            ),
            (
                "negateFloat#",
                vec![RuntimeRep::Float(32)],
                vec![RuntimeRep::Float(64)],
            ),
        ] {
            assert!(FloatingFamily::recognize(
                &OperationIdentity::PrimOp(name.into()),
                &Signature {
                    arguments,
                    results: ResultContract::Returns(results),
                }
            )
            .is_none());
        }
    }
}
