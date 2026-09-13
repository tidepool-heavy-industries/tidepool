//! End-to-end generated Tail-ABI application coverage.

use super::{CompiledProgram, ExecutionError, ObservationFailure, RunOptions};
use crate::host_fns::RuntimeError;
use crate::prepared_control::PreparedSafepoint;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_heap::execution_descriptor::ObjectKind;
use tidepool_repr::execution_schema::{testing, *};

fn constructor(index: u64) -> ConstructorDecl {
    ConstructorDecl {
        identity: testing::identity("W5Apply", &format!("C{index}")),
        family: testing::identity("W5Apply", &format!("T{index}")),
        host_id: tidepool_repr::DataConId(980 + index),
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
        strict_fields: vec![],
        field_reps: vec![],
        layout: CheckedLayout {
            fields: vec![],
            alignment: 1,
            payload_size: 0,
            root_mask: vec![],
        },
    }
}

fn compile(wire: WireProgram) -> CompiledProgram {
    let prepared = testing::prepare(wire).expect("apply fixture validates");
    let linked = link_program(prepared, &MachineImports::default()).expect("apply fixture links");
    CompiledProgram::compile(&linked).expect("apply fixture compiles")
}

fn ref_atom(id: u32) -> Atom {
    Atom::Ref(ValueRef::Local(ValueId(id)))
}

fn partial_wire(exact: bool) -> WireProgram {
    let mut wire = testing::wire_program();
    // entry, two-argument function, and one-argument demanded application.
    wire.signatures = vec![
        Signature {
            arguments: vec![],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        },
    ];
    wire.constructors = vec![constructor(0), constructor(1)];
    // Function 1 returns its second argument. The entry first makes a PAP
    // and, for the exact case, enters it through the same dispatcher.
    wire.expressions.nodes = vec![
        ExprFrame::Return(vec![ref_atom(101)]),
        ExprFrame::Call {
            callee: ref_atom(1),
            signature: SignatureId(2),
            arguments: vec![ref_atom(10)],
        },
    ];
    let entry_body = if exact {
        wire.expressions.nodes.push(ExprFrame::Call {
            callee: ref_atom(12),
            signature: SignatureId(2),
            arguments: vec![ref_atom(11)],
        });
        wire.expressions.nodes.push(ExprFrame::Case {
            scrutinee: 1,
            binder: ValueId(12),
            scrutinee_reps: vec![RuntimeRep::LiftedRef],
            kind: CaseKind::Polymorphic,
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: 2,
            }],
        });
        3
    } else {
        1
    };
    wire.expressions.nodes.push(ExprFrame::Let {
        bindings: Group::NonRecursive(HeapBinding {
            id: ValueId(11),
            rhs: HeapRhs::Constructor {
                constructor: ConstructorId(1),
                fields: vec![],
            },
        }),
        body: entry_body,
    });
    let y_bound = wire.expressions.nodes.len() - 1;
    wire.expressions.nodes.push(ExprFrame::Let {
        bindings: Group::NonRecursive(HeapBinding {
            id: ValueId(10),
            rhs: HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: vec![],
            },
        }),
        body: y_bound,
    });
    let entry_body = wire.expressions.nodes.len() - 1;
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5Apply", "entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![],
                    captures: vec![],
                    body: entry_body,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5Apply", "two"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![ValueId(100), ValueId(101)],
                    captures: vec![],
                    body: 0,
                },
            },
        },
    ])];
    wire
}

#[test]
fn pap_undersaturation_allocates() {
    let error = compile(partial_wire(false))
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect_err("the adapter must receive the allocated PAP");
    assert!(matches!(
        error,
        ExecutionError::Observation(ObservationFailure::Unobservable(ObjectKind::Pap))
    ));
}

#[test]
fn pap_allocation_cancellation_publishes_no_result() {
    let program = compile(partial_wire(false));
    let failure = super::invocation::PreparedInvocation::enter_with_injected_poll_failure(
        &program,
        ValueId(0),
        &[],
        &RunOptions {
            // The two local constructors fit; the PAP reserve takes the
            // allocation slow path where the injected cancellation fires.
            nursery_bytes: 48,
            ..RunOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        (PreparedSafepoint::Allocation, 1, RuntimeError::Cancelled),
    );
    assert!(matches!(
        failure,
        Err(ExecutionError::Runtime(error)) if error.cause == RuntimeError::Cancelled
    ));
    // A fresh invocation proves the failed one did not publish a partial PAP
    // or poison the immutable program owner.
    let retry = program.run_entry(
        ValueId(0),
        &[],
        &RunOptions::default(),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(matches!(
        retry,
        Err(ExecutionError::Observation(
            ObservationFailure::Unobservable(ObjectKind::Pap)
        ))
    ));
}

#[test]
fn pap_exact_application_calls() {
    let result = compile(partial_wire(true))
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("the exact PAP call completes through the generated adapter");
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::Value::Con(id, fields)]
            if *id == tidepool_repr::DataConId(981) && fields.is_empty()
    ));
}

#[test]
fn pap_partial_to_partial_flattens_mixed_prefix_across_collection() {
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![
                RuntimeRep::LiftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Void,
                RuntimeRep::LiftedRef,
            ],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::Void],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        },
    ];
    wire.constructors = vec![constructor(3), constructor(4), constructor(5)];
    wire.expressions.nodes = vec![
        ExprFrame::Return(vec![ref_atom(104)]),
        ExprFrame::Call {
            callee: ref_atom(1),
            signature: SignatureId(2),
            arguments: vec![
                ref_atom(10),
                Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 7_i64.to_be_bytes().to_vec(),
                }),
            ],
        },
        ExprFrame::Call {
            callee: ref_atom(12),
            signature: SignatureId(3),
            arguments: vec![Atom::Void],
        },
        ExprFrame::Case {
            scrutinee: 1,
            binder: ValueId(12),
            scrutinee_reps: vec![RuntimeRep::LiftedRef],
            kind: CaseKind::Polymorphic,
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: 2,
            }],
        },
        ExprFrame::Call {
            callee: ref_atom(13),
            signature: SignatureId(4),
            arguments: vec![ref_atom(11)],
        },
        ExprFrame::Case {
            scrutinee: 3,
            binder: ValueId(13),
            scrutinee_reps: vec![RuntimeRep::LiftedRef],
            kind: CaseKind::Polymorphic,
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: 4,
            }],
        },
        ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(11),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(2),
                    fields: vec![],
                },
            }),
            body: 5,
        },
        ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(10),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            }),
            body: 6,
        },
    ];
    let mut body = 7;
    for id in 200..232 {
        wire.expressions.nodes.push(ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(id),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(1),
                    fields: vec![],
                },
            }),
            body,
        });
        body = wire.expressions.nodes.len() - 1;
    }
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5Apply", "mixed-entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![],
                    captures: vec![],
                    body,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5Apply", "mixed-four"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![ValueId(101), ValueId(102), ValueId(103), ValueId(104)],
                    captures: vec![],
                    body: 0,
                },
            },
        },
    ])];
    let result = compile(wire)
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 64,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .expect("flattened PAP prefix completes after moving collection");
    assert!(result.collections > 0);
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::Value::Con(id, fields)]
            if *id == tidepool_repr::DataConId(985) && fields.is_empty()
    ));
}

#[test]
fn pap_oversaturation_applies_remainder() {
    let mut wire = testing::wire_program();
    // `one` consumes Int64 and returns `two`; the initial call supplies an
    // additional Word64. Its `[Word64] -> LiftedRef` suffix has no wire
    // signature, so declaration must close the demand set itself.
    wire.signatures = vec![
        Signature {
            arguments: vec![],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::Word(64), RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        },
    ];
    wire.constructors = vec![constructor(2)];
    wire.expressions.nodes = vec![
        ExprFrame::Return(vec![ref_atom(2)]),
        ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        },
        ExprFrame::Call {
            callee: ref_atom(1),
            signature: SignatureId(3),
            arguments: vec![
                Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 7_i64.to_be_bytes().to_vec(),
                }),
                Atom::Scalar(ScalarLiteral::Word {
                    bits: 64,
                    bytes: 9_u64.to_be_bytes().to_vec(),
                }),
            ],
        },
        ExprFrame::Call {
            callee: ref_atom(20),
            signature: SignatureId(4),
            arguments: vec![ref_atom(21)],
        },
        ExprFrame::Case {
            scrutinee: 2,
            binder: ValueId(20),
            scrutinee_reps: vec![RuntimeRep::LiftedRef],
            kind: CaseKind::Polymorphic,
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: 3,
            }],
        },
        ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(21),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            }),
            body: 4,
        },
    ];
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5Apply", "entry-over"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![],
                    captures: vec![],
                    body: 5,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5Apply", "one"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![ValueId(30)],
                    captures: vec![ValueRef::Local(ValueId(2))],
                    body: 0,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5Apply", "two"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Function {
                    signature: SignatureId(2),
                    parameters: vec![ValueId(31), ValueId(32)],
                    captures: vec![],
                    body: 1,
                },
            },
        },
    ])];
    let result = compile(wire)
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("oversaturation must route the generated suffix dispatcher");
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::Value::Con(id, fields)]
            if *id == tidepool_repr::DataConId(982) && fields.is_empty()
    ));
}

#[test]
fn pap_oversaturation_enters_a_thunk_result_before_suffix_dispatch() {
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::Word(64), RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
            results: vec![RuntimeRep::LiftedRef],
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: vec![RuntimeRep::LiftedRef],
        },
    ];
    wire.constructors = vec![constructor(6)];
    wire.expressions.nodes = vec![
        ExprFrame::Return(vec![ref_atom(3)]),
        ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        },
        ExprFrame::Return(vec![ref_atom(2)]),
        ExprFrame::Call {
            callee: ref_atom(1),
            signature: SignatureId(3),
            arguments: vec![
                Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 1_i64.to_be_bytes().to_vec(),
                }),
                Atom::Scalar(ScalarLiteral::Word {
                    bits: 64,
                    bytes: 2_u64.to_be_bytes().to_vec(),
                }),
            ],
        },
        ExprFrame::Call {
            callee: ref_atom(20),
            signature: SignatureId(4),
            arguments: vec![ref_atom(21)],
        },
        ExprFrame::Case {
            scrutinee: 3,
            binder: ValueId(20),
            scrutinee_reps: vec![RuntimeRep::LiftedRef],
            kind: CaseKind::Polymorphic,
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: 4,
            }],
        },
        ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(21),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            }),
            body: 5,
        },
    ];
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5Apply", "thunk-over-entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![],
                    captures: vec![],
                    body: 6,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5Apply", "thunk-over-one"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![ValueId(30)],
                    captures: vec![ValueRef::Local(ValueId(3))],
                    body: 0,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5Apply", "thunk-over-two"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Function {
                    signature: SignatureId(2),
                    parameters: vec![ValueId(31), ValueId(32)],
                    captures: vec![],
                    body: 1,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5Apply", "thunk-over-result"),
            binding: HeapBinding {
                id: ValueId(3),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Local(ValueId(2))],
                    body: 2,
                },
            },
        },
    ])];
    let result = compile(wire)
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("oversaturation suffix enters the returned thunk");
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::Value::Con(id, fields)]
            if *id == tidepool_repr::DataConId(986) && fields.is_empty()
    ));
}
