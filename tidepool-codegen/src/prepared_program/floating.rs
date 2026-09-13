//! Pure floating operations retain GHC identities and exact representations.

use cranelift_codegen::ir::{InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_repr::execution_schema::{ForeignConvention, OperationIdentity, RuntimeRep, Signature};
use super::primitives::ScalarFamily;

pub(super) struct FloatingFamily;

#[derive(Clone, Copy)]
pub(super) enum FloatingOperation {
    NearestDouble,
}

impl ScalarFamily for FloatingFamily {
    type Operation = FloatingOperation;

    /// This C symbol has no Haskell unfolding: ghc-internal implements it in
    /// C as ties-even rounding. Cranelift nearest implements that operation,
    /// preserving signed zero and IEEE exceptional values without a host call.
    /// A similarly named primop or a different signature has no such authority.
    fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<Self::Operation> {
        match identity {
            OperationIdentity::Intrinsic { symbol, convention: ForeignConvention::CCall }
                if symbol == "rintDouble"
                    && signature.arguments == [RuntimeRep::Float(64)]
                    && signature.results == [RuntimeRep::Float(64)] => Some(FloatingOperation::NearestDouble),
            _ => None,
        }
    }

    fn emit(operation: Self::Operation, builder: &mut FunctionBuilder<'_>, arguments: &[Value]) -> Vec<Value> {
        match operation {
            FloatingOperation::NearestDouble => vec![builder.ins().nearest(arguments[0])],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::{testing, *};

    #[test]
    fn w5_a3_rint_double_exact_identity_rounds_ties_even() {
        for (input, expected) in [(0.5_f64, 0.0_f64), (1.5, 2.0), (-0.5, -0.0), (-1.5, -2.0)] {
            let mut wire = testing::wire_program();
            wire.signatures[0].results = vec![RuntimeRep::Float(64)];
            wire.signatures.push(Signature {
                arguments: vec![RuntimeRep::Float(64)], results: vec![RuntimeRep::Float(64)],
            });
            wire.operations.push(OperationDecl {
                identity: OperationIdentity::Intrinsic { symbol: "rintDouble".into(), convention: ForeignConvention::CCall },
                signature: SignatureId(1),
            });
            wire.expressions.nodes[0] = ExprFrame::Operation {
                operation: OperationId(0), arguments: vec![Atom::Scalar(ScalarLiteral::Float {
                    bits: 64, bytes: input.to_bits().to_be_bytes().to_vec(),
                })],
            };
            let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
            let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
            let result = compiled.run_entry(ValueId(0), &[], &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false))).unwrap();
            assert!(matches!(result.values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                if *bits == expected.to_bits()));
        }
    }
}
