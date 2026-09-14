use super::{CompiledProgram, RunOptions, TopSlotBase};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_repr::execution_schema::{testing, *};

fn integer(value: i64) -> Atom {
    Atom::Scalar(ScalarLiteral::Int {
        bits: 64,
        bytes: value.to_be_bytes().to_vec(),
    })
}

fn word8(value: u8) -> Atom {
    Atom::Scalar(ScalarLiteral::Word {
        bits: 8,
        bytes: vec![value],
    })
}

fn local(id: u32) -> Atom {
    Atom::Ref(ValueRef::Local(ValueId(id)))
}

fn operation(operation: u32, arguments: Vec<Atom>) -> ExprFrame<usize> {
    ExprFrame::Operation {
        operation: OperationId(operation),
        arguments,
    }
}

fn multivalue_case(
    scrutinee: usize,
    binder: u32,
    results: Vec<RuntimeRep>,
    binders: Vec<ValueId>,
    body: usize,
) -> ExprFrame<usize> {
    ExprFrame::Case {
        scrutinee,
        binder: ValueId(binder),
        scrutinee_results: ResultContract::Returns(results),
        kind: CaseKind::MultiValue,
        alternatives: vec![Alternative {
            pattern: AlternativePattern::Default,
            binders,
            body,
        }],
    }
}

fn owner_constructor() -> ConstructorDecl {
    let reps = vec![RuntimeRep::UnliftedRef];
    let layout = StorageLayout::for_reps(&testing::target(), &reps).unwrap();
    ConstructorDecl {
        identity: testing::identity("Lifetime", "Owner"),
        family: testing::identity("Lifetime", "OwnerT"),
        host_id: tidepool_repr::DataConId(991),
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
        strict_fields: vec![false],
        field_reps: reps,
        layout: CheckedLayout {
            fields: layout
                .fields()
                .iter()
                .map(|field| FieldLayout {
                    rep: field.rep(),
                    offset: field.offset(),
                })
                .collect(),
            alignment: layout.alignment(),
            payload_size: layout.payload_size(),
            root_mask: vec![true],
        },
    }
}

fn keep_alive_wire(garbage_allocations: usize) -> WireProgram {
    let reference = RuntimeRep::UnliftedRef;
    let int = RuntimeRep::Int(64);
    let word = RuntimeRep::Word(8);
    let address = RuntimeRep::Address;
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![word]),
        },
        Signature {
            arguments: vec![int, RuntimeRep::Void],
            results: ResultContract::Returns(vec![reference]),
        },
        Signature {
            arguments: vec![reference, int, word, RuntimeRep::Void],
            results: ResultContract::Returns(vec![]),
        },
        Signature {
            arguments: vec![reference],
            results: ResultContract::Returns(vec![address]),
        },
        Signature {
            arguments: vec![RuntimeRep::Void],
            results: ResultContract::Returns(vec![word]),
        },
        Signature {
            arguments: vec![address, int, RuntimeRep::Void],
            results: ResultContract::Returns(vec![word]),
        },
        Signature {
            arguments: vec![reference, RuntimeRep::Void, RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![word]),
        },
    ];
    wire.operations = [
        ("newPinnedByteArray#", 1),
        ("writeWord8Array#", 2),
        ("mutableByteArrayContents#", 3),
        ("newByteArray#", 1),
        ("readWord8OffAddr#", 5),
        ("keepAlive#", 6),
    ]
    .into_iter()
    .map(|(name, signature)| OperationDecl {
        identity: OperationIdentity::PrimOp(name.into()),
        signature: SignatureId(signature),
    })
    .collect();

    // The callback's only capture is the scalar Addr#. Its allocation churn
    // forces a moving collection before dereferencing that address.
    let mut nodes = vec![
        operation(4, vec![local(20), integer(0), Atom::Void]),
        ExprFrame::Return(vec![local(40)]),
        multivalue_case(0, 140, vec![word], vec![ValueId(40)], 1),
    ];
    let mut callback_body = 2;
    for index in 0..garbage_allocations {
        let allocation = nodes.len();
        nodes.push(operation(3, vec![integer(8), Atom::Void]));
        let case = nodes.len();
        nodes.push(multivalue_case(
            allocation,
            200 + index as u32,
            vec![reference],
            vec![ValueId(300 + index as u32)],
            callback_body,
        ));
        callback_body = case;
    }

    let new_pinned = nodes.len();
    nodes.push(operation(0, vec![integer(1), Atom::Void]));
    let write_marker = nodes.len();
    nodes.push(operation(
        1,
        vec![local(10), integer(0), word8(0x7b), Atom::Void],
    ));
    let contents = nodes.len();
    nodes.push(operation(2, vec![local(10)]));
    let keep_alive = nodes.len();
    nodes.push(operation(5, vec![local(10), Atom::Void, local(21)]));
    let return_marker = nodes.len();
    nodes.push(ExprFrame::Return(vec![local(50)]));
    let keep_alive_case = nodes.len();
    nodes.push(multivalue_case(
        keep_alive,
        150,
        vec![word],
        vec![ValueId(50)],
        return_marker,
    ));
    let callback = nodes.len();
    nodes.push(ExprFrame::Let {
        bindings: Group::NonRecursive(HeapBinding {
            id: ValueId(21),
            rhs: HeapRhs::Function {
                signature: SignatureId(4),
                parameters: vec![ValueId(30)],
                captures: vec![ValueRef::Local(ValueId(20))],
                body: callback_body,
            },
        }),
        body: keep_alive_case,
    });
    let contents_case = nodes.len();
    nodes.push(multivalue_case(
        contents,
        151,
        vec![address],
        vec![ValueId(20)],
        callback,
    ));
    let write_case = nodes.len();
    nodes.push(multivalue_case(
        write_marker,
        152,
        vec![],
        vec![],
        contents_case,
    ));
    let entry_body = nodes.len();
    nodes.push(multivalue_case(
        new_pinned,
        153,
        vec![reference],
        vec![ValueId(10)],
        write_case,
    ));
    wire.expressions.nodes = nodes;
    wire.bindings = vec![Group::NonRecursive(TopBinding {
        identity: testing::identity("Lifetime", "entry"),
        binding: HeapBinding {
            id: ValueId(0),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![],
                captures: vec![],
                body: entry_body,
            },
        },
    })];
    wire
}

fn touch_wire(garbage_allocations: usize) -> WireProgram {
    let reference = RuntimeRep::UnliftedRef;
    let int = RuntimeRep::Int(64);
    let word = RuntimeRep::Word(8);
    let address = RuntimeRep::Address;
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![word]),
        },
        Signature {
            arguments: vec![int, RuntimeRep::Void],
            results: ResultContract::Returns(vec![reference]),
        },
        Signature {
            arguments: vec![reference, int, word, RuntimeRep::Void],
            results: ResultContract::Returns(vec![]),
        },
        Signature {
            arguments: vec![reference],
            results: ResultContract::Returns(vec![address]),
        },
        Signature {
            arguments: vec![RuntimeRep::Void],
            results: ResultContract::Returns(vec![word]),
        },
        Signature {
            arguments: vec![address, int, RuntimeRep::Void],
            results: ResultContract::Returns(vec![word]),
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Void],
            results: ResultContract::Returns(vec![]),
        },
    ];
    wire.operations = [
        ("newPinnedByteArray#", 1),
        ("writeWord8Array#", 2),
        ("mutableByteArrayContents#", 3),
        ("newByteArray#", 1),
        ("readWord8OffAddr#", 5),
        ("touch#", 6),
    ]
    .into_iter()
    .map(|(name, signature)| OperationDecl {
        identity: OperationIdentity::PrimOp(name.into()),
        signature: SignatureId(signature),
    })
    .collect();
    wire.constructors.push(owner_constructor());

    // The callback captures only the raw address and allocates enough garbage
    // to move the owner. The later touch is the sole managed use keeping the
    // lifted holder, and therefore its byte-array field, live across that call.
    let mut nodes = vec![
        operation(4, vec![local(20), integer(0), Atom::Void]),
        ExprFrame::Return(vec![local(40)]),
        multivalue_case(0, 140, vec![word], vec![ValueId(40)], 1),
    ];
    let mut callback_body = 2;
    for index in 0..garbage_allocations {
        let allocation = nodes.len();
        nodes.push(operation(3, vec![integer(8), Atom::Void]));
        let case = nodes.len();
        nodes.push(multivalue_case(
            allocation,
            200 + index as u32,
            vec![reference],
            vec![ValueId(300 + index as u32)],
            callback_body,
        ));
        callback_body = case;
    }

    let new_pinned = nodes.len();
    nodes.push(operation(0, vec![integer(1), Atom::Void]));
    let write_marker = nodes.len();
    nodes.push(operation(
        1,
        vec![local(10), integer(0), word8(0x7b), Atom::Void],
    ));
    let contents = nodes.len();
    nodes.push(operation(2, vec![local(10)]));
    let construct_owner = nodes.len();
    nodes.push(ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: vec![local(10)],
    });
    let callback_call = nodes.len();
    nodes.push(ExprFrame::Call {
        callee: local(21),
        signature: SignatureId(4),
        arguments: vec![Atom::Void],
    });
    let touch = nodes.len();
    nodes.push(operation(5, vec![local(11), Atom::Void]));
    let return_marker = nodes.len();
    nodes.push(ExprFrame::Return(vec![local(50)]));
    let touch_case = nodes.len();
    nodes.push(multivalue_case(touch, 150, vec![], vec![], return_marker));
    let callback_case = nodes.len();
    nodes.push(multivalue_case(
        callback_call,
        151,
        vec![word],
        vec![ValueId(50)],
        touch_case,
    ));
    let callback = nodes.len();
    nodes.push(ExprFrame::Let {
        bindings: Group::NonRecursive(HeapBinding {
            id: ValueId(21),
            rhs: HeapRhs::Function {
                signature: SignatureId(4),
                parameters: vec![ValueId(30)],
                captures: vec![ValueRef::Local(ValueId(20))],
                body: callback_body,
            },
        }),
        body: callback_case,
    });
    let owner_case = nodes.len();
    nodes.push(multivalue_case(
        construct_owner,
        152,
        vec![RuntimeRep::LiftedRef],
        vec![ValueId(11)],
        callback,
    ));
    let contents_case = nodes.len();
    nodes.push(multivalue_case(
        contents,
        153,
        vec![address],
        vec![ValueId(20)],
        owner_case,
    ));
    let write_case = nodes.len();
    nodes.push(multivalue_case(
        write_marker,
        154,
        vec![],
        vec![],
        contents_case,
    ));
    let entry_body = nodes.len();
    nodes.push(multivalue_case(
        new_pinned,
        155,
        vec![reference],
        vec![ValueId(10)],
        write_case,
    ));
    wire.expressions.nodes = nodes;
    wire.bindings = vec![Group::NonRecursive(TopBinding {
        identity: testing::identity("Lifetime", "touchEntry"),
        binding: HeapBinding {
            id: ValueId(0),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![],
                captures: vec![],
                body: entry_body,
            },
        },
    })];
    wire
}

#[test]
fn keep_alive_retains_external_bytes_during_callback_gc() {
    let prepared = testing::prepare(keep_alive_wire(16)).expect("lifetime fixture validates");
    let linked = link_program(prepared, &MachineImports::default()).expect("fixture links");
    let program = CompiledProgram::compile(&linked, TopSlotBase::ZERO).expect("fixture compiles");
    let result = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 64,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .expect("keepAlive must retain the byte owner through callback collection");

    assert!(result.collections > 0, "callback allocation must collect");
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::Value::Lit(
            tidepool_repr::Literal::LitWord(0x7b)
        )]
    ));
}

#[test]
fn touch_retains_external_bytes_across_preceding_callback_gc() {
    let prepared = testing::prepare(touch_wire(16)).expect("touch fixture validates");
    let linked = link_program(prepared, &MachineImports::default()).expect("fixture links");
    let program = CompiledProgram::compile(&linked, TopSlotBase::ZERO).expect("fixture compiles");
    let result = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 64,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .expect("touch must retain the byte owner through preceding callback collection");

    assert!(result.collections > 0, "callback allocation must collect");
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::Value::Lit(
            tidepool_repr::Literal::LitWord(0x7b)
        )]
    ));
}

#[test]
fn keep_alive_recognition_requires_exact_arguments_and_a_returning_contract() {
    let declaration = OperationDecl {
        identity: OperationIdentity::PrimOp("keepAlive#".into()),
        signature: SignatureId(0),
    };
    let returning = Signature {
        arguments: vec![
            RuntimeRep::UnliftedRef,
            RuntimeRep::Void,
            RuntimeRep::LiftedRef,
        ],
        results: ResultContract::Returns(vec![]),
    };
    assert_eq!(
        super::lifetime::callback_signature(&declaration, &returning),
        Some(Signature {
            arguments: vec![RuntimeRep::Void],
            results: ResultContract::Returns(vec![]),
        })
    );

    let mut no_success = returning.clone();
    no_success.results = ResultContract::NoSuccess;
    assert!(super::lifetime::callback_signature(&declaration, &no_success).is_none());

    for arguments in [
        vec![RuntimeRep::UnliftedRef, RuntimeRep::LiftedRef],
        vec![
            RuntimeRep::UnliftedRef,
            RuntimeRep::Void,
            RuntimeRep::UnliftedRef,
        ],
        vec![RuntimeRep::Address, RuntimeRep::Void, RuntimeRep::LiftedRef],
        vec![
            RuntimeRep::UnliftedRef,
            RuntimeRep::Void,
            RuntimeRep::LiftedRef,
            RuntimeRep::Void,
        ],
    ] {
        let malformed = Signature {
            arguments,
            results: ResultContract::Returns(vec![]),
        };
        assert!(super::lifetime::callback_signature(&declaration, &malformed).is_none());
    }
}

#[test]
fn touch_recognition_requires_exact_identity_and_signature() {
    let accepted = Signature {
        arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Void],
        results: ResultContract::Returns(vec![]),
    };
    assert!(super::lifetime::recognize_touch(
        &OperationIdentity::PrimOp("touch#".into()),
        &accepted,
    ));
    for rejected in [
        Signature {
            arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
            ..accepted.clone()
        },
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            ..accepted.clone()
        },
        Signature {
            results: ResultContract::NoSuccess,
            ..accepted.clone()
        },
    ] {
        assert!(!super::lifetime::recognize_touch(
            &OperationIdentity::PrimOp("touch#".into()),
            &rejected,
        ));
    }
    assert!(!super::lifetime::recognize_touch(
        &OperationIdentity::PrimOp("touch#lookalike".into()),
        &accepted,
    ));
}

#[test]
fn prepared_admission_rejects_no_success_keep_alive() {
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::LiftedRef],
            results: ResultContract::NoSuccess,
        },
        Signature {
            arguments: vec![
                RuntimeRep::UnliftedRef,
                RuntimeRep::Void,
                RuntimeRep::LiftedRef,
            ],
            results: ResultContract::NoSuccess,
        },
    ];
    wire.operations = vec![OperationDecl {
        identity: OperationIdentity::PrimOp("keepAlive#".into()),
        signature: SignatureId(1),
    }];
    wire.expressions.nodes = vec![
        operation(0, vec![local(10), Atom::Void, local(11)]),
        ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(12),
            scrutinee_results: ResultContract::NoSuccess,
            kind: CaseKind::MultiValue,
            alternatives: vec![],
        },
    ];
    wire.bindings = vec![Group::NonRecursive(TopBinding {
        identity: testing::identity("Lifetime", "bottoming"),
        binding: HeapBinding {
            id: ValueId(0),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(10), ValueId(11)],
                captures: vec![],
                body: 1,
            },
        },
    })];

    let prepared = testing::prepare(wire).expect("bottoming keepAlive fixture validates");
    assert!(matches!(
        super::admit_prepared(&prepared),
        Err(super::Unsupported::Operation {
            node: 0,
            identity: OperationIdentity::PrimOp(name),
            signature: Signature {
                arguments,
                results: ResultContract::NoSuccess,
            },
            ..
        }) if name == "keepAlive#"
            && arguments == [
                RuntimeRep::UnliftedRef,
                RuntimeRep::Void,
                RuntimeRep::LiftedRef,
            ]
    ));
}
