use super::{CompiledProgram, RunOptions};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_repr::execution_schema::{testing, *};

fn caf_program(garbage_objects: u32) -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = vec![RuntimeRep::LiftedRef];
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("W5", "Unit"),
        family: testing::identity("W5", "Unit"),
        host_id: tidepool_repr::DataConId(900),
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
        field_reps: vec![],
        strict_fields: vec![],
        layout: CheckedLayout {
            fields: vec![],
            alignment: 8,
            payload_size: 0,
            root_mask: vec![],
        },
    });
    wire.expressions.nodes[0] = ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: vec![],
    };
    let mut body = 0;
    for index in 0..garbage_objects {
        wire.expressions.nodes.push(ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(index + 1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            }),
            body,
        });
        body = wire.expressions.nodes.len() - 1;
    }
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        unreachable!()
    };
    top.binding.rhs = HeapRhs::Thunk {
        signature: SignatureId(0),
        update: UpdatePolicy::Memoize,
        captures: vec![],
        body,
    };
    let prepared = testing::prepare(wire).unwrap();
    let linked = link_program(prepared, &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

/// The real adapter must enter a heap CAF, allocate its value and settle it.
#[test]
fn w5_a1_top_thunk_constructs_and_observes() {
    let program = caf_program(0);
    let result = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(result.values.len(), 1);
    assert!(
        matches!(&result.values[0], tidepool_bridge::Value::Con(id, fields) if *id == tidepool_repr::DataConId(900) && fields.is_empty())
    );
}

#[test]
fn w5_a1_collection_during_thunk_body_keeps_update_root_live() {
    let program = caf_program(32);
    let options = RunOptions {
        nursery_bytes: 64,
        ..RunOptions::default()
    };
    let result = program
        .run_entry(ValueId(0), &[], &options, Arc::new(AtomicBool::new(false)))
        .unwrap();
    assert!(
        result.collections > 0,
        "collection must happen during generated body execution"
    );
    assert!(
        matches!(&result.values[0], tidepool_bridge::Value::Con(id, fields)
        if *id == tidepool_repr::DataConId(900) && fields.is_empty())
    );
}
