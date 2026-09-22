use super::{CompileError, CompiledProgram, ExecutionError, RunOptions, Unsupported};
use crate::host_fns::RuntimeError;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_repr::execution_schema::{self, *};

fn double2int_wire(entry_rep: RuntimeRep, operation_rep: RuntimeRep) -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![entry_rep],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        },
        Signature {
            arguments: vec![operation_rep],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        },
    ];
    wire.operations = vec![OperationDecl {
        identity: OperationIdentity::PrimOp("double2Int#".into()),
        signature: SignatureId(1),
    }];
    wire.expressions.nodes[0] = ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
    };
    wire.bindings = vec![Group::NonRecursive(TopBinding {
        identity: testing::identity("DoubleToInt", "entry"),
        binding: HeapBinding {
            id: ValueId(0),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(1)],
                captures: vec![],
                body: 0,
            },
        },
    })];
    wire
}

fn compile(entry_rep: RuntimeRep, operation_rep: RuntimeRep) -> CompiledProgram {
    let prepared = testing::prepare(double2int_wire(entry_rep, operation_rep)).unwrap();
    let linked = execution_schema::link_program(prepared, &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).expect("double2Int# fixture compiles")
}

fn run(program: &CompiledProgram, value: f64) -> Result<super::RunResult, ExecutionError> {
    program.run_entry(
        ValueId(0),
        &[value.to_bits()],
        &RunOptions::default(),
        Arc::new(AtomicBool::new(false)),
    )
}

fn assert_overflow(result: Result<super::RunResult, ExecutionError>) {
    assert!(matches!(
        result,
        Err(ExecutionError::Runtime(
            crate::machine_state::MachineFailure {
                cause: RuntimeError::Overflow,
                ..
            }
        ))
    ));
}

#[test]
fn double2int_truncates_finite_values_and_preserves_signed_zero() {
    let program = compile(RuntimeRep::Float(64), RuntimeRep::Float(64));
    for (value, expected) in [(3.75, 3), (-3.75, -3), (0.0, 0), (-0.0, 0)] {
        let result = run(&program, value).unwrap();
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(actual))]
                if *actual == expected
        ));
    }
}

#[test]
fn double2int_accepts_lower_bound_and_rejects_exclusive_upper_bound() {
    let program = compile(RuntimeRep::Float(64), RuntimeRep::Float(64));
    let lower = run(&program, -9_223_372_036_854_775_808.0).unwrap();
    assert!(matches!(
        lower.values.as_slice(),
        [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(actual))]
            if *actual == i64::MIN
    ));

    // The next representable f64 below 2^63 is in range and truncates to its
    // exact integer value; the bound itself is rejected because it is exclusive.
    let just_below = run(&program, f64::from_bits(0x43dfffffffffffff)).unwrap();
    assert!(matches!(
        just_below.values.as_slice(),
        [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(actual))]
            if *actual == 9_223_372_036_854_774_784_i64
    ));
    assert_overflow(run(&program, 9_223_372_036_854_775_808.0));
    assert_overflow(run(&program, f64::from_bits(0xc3e0000000000001)));
}

#[test]
fn double2int_non_finite_values_report_reusable_overflow() {
    let program = compile(RuntimeRep::Float(64), RuntimeRep::Float(64));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_overflow(run(&program, value));
    }
    // A failed invocation must not poison the immutable compiled owner or the
    // next invocation's first-cause state.
    let valid = run(&program, 12.5).unwrap();
    assert!(matches!(
        valid.values.as_slice(),
        [tidepool_bridge::HaskellValue::Lit(
            tidepool_repr::Literal::LitInt(12)
        )]
    ));
    assert_overflow(run(&program, f64::NAN));
}

#[test]
fn double2int_rejects_wrong_signature_before_native_emission() {
    let wire = double2int_wire(RuntimeRep::Float(32), RuntimeRep::Float(32));
    let prepared = testing::prepare(wire).unwrap();
    let linked = execution_schema::link_program(prepared, &MachineImports::default()).unwrap();
    let result = CompiledProgram::compile(&linked);
    assert!(matches!(
        result,
        Err(CompileError::Unsupported(Unsupported::Operation { .. }))
    ));
}
