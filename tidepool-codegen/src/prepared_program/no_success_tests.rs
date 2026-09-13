use super::{CompiledProgram, ExecutionError, RunOptions};
use crate::host_fns::RuntimeError;
use crate::machine_state::{MachineDisposition, MachineFailure};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_repr::execution_schema::{testing, *};

/// A real raised CAF: its exception is an ordinary owned constructor, not a
/// fake pointer or a diagnostic string manufactured by the runtime.
fn raised_caf() -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::NoSuccess;
    wire.signatures.push(Signature {
        arguments: vec![RuntimeRep::LiftedRef],
        results: ResultContract::NoSuccess,
    });
    wire.operations.push(OperationDecl {
        identity: OperationIdentity::PrimOp("raise#".into()),
        signature: SignatureId(1),
    });
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("Failure", "Exception"),
        host_id: tidepool_repr::DataConId(971),
        family: testing::identity("Failure", "ExceptionType"),
        result_rep: RuntimeRep::LiftedRef,
        field_reps: vec![],
        strict_fields: vec![],
        layout: CheckedLayout {
            fields: vec![], alignment: 1, payload_size: 0, root_mask: vec![],
        },
        tag: 1,
        family_size: 1,
    });
    wire.expressions.nodes[0] = ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
    };
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("Failure", "exception"),
            binding: HeapBinding { id: ValueId(1), rhs: HeapRhs::Constructor {
                constructor: ConstructorId(0), fields: vec![],
            } },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("Failure", "raisedCaf"),
            binding: HeapBinding { id: ValueId(0), rhs: HeapRhs::Thunk {
                signature: SignatureId(0), update: UpdatePolicy::Memoize,
                captures: vec![], body: 0,
            } },
        }),
    ];
    wire
}

#[test]
fn w5_no_success_raised_caf_uses_status_only_body_and_reusable_settlement() {
    let linked = link_program(testing::prepare(raised_caf()).unwrap(), &MachineImports::default()).unwrap();
    let program = CompiledProgram::compile(&linked).unwrap();
    for _ in 0..2 {
        let result = program.run_entry(ValueId(0), &[], &RunOptions::default(), Arc::new(AtomicBool::new(false)));
        assert!(matches!(result, Err(ExecutionError::Runtime(MachineFailure {
            cause: RuntimeError::RaisedException,
            disposition: MachineDisposition::Reusable,
        }))));
    }
}
