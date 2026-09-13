use super::{CompiledProgram, DescriptorMeaning, ExecutionError, RunOptions};
use crate::host_fns::RuntimeError;
use crate::machine_state::{MachineDisposition, MachineFailure};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_heap::execution_descriptor::DescriptorState;
use tidepool_heap::managed_reference::untag;
use tidepool_repr::execution_schema::{testing, *};

#[test]
fn raising_arithmetic_primops_return_typed_reusable_failure() {
    for (name, cause) in [
        ("raiseDivZero#", RuntimeError::DivisionByZero),
        ("raiseUnderflow#", RuntimeError::Underflow),
    ] {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::NoSuccess;
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Void],
            results: ResultContract::NoSuccess,
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![Atom::Void],
        };
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        let program = CompiledProgram::compile(&linked).unwrap();
        assert!(matches!(
            program.run_entry(ValueId(0), &[], &RunOptions::default(), Arc::new(AtomicBool::new(false))),
            Err(ExecutionError::Runtime(MachineFailure {
                cause: actual, disposition: MachineDisposition::Reusable,
            })) if actual == cause
        ));
    }
}

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
            fields: vec![],
            alignment: 1,
            payload_size: 0,
            root_mask: vec![],
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
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("Failure", "raisedCaf"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: 0,
                },
            },
        }),
    ];
    wire
}

fn raised_caf_reference_entry() -> WireProgram {
    let mut wire = raised_caf();
    let entry_signature = SignatureId(wire.signatures.len() as u32);
    wire.signatures.push(Signature {
        arguments: vec![],
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    });
    let Group::NonRecursive(caf) = &mut wire.bindings[1] else {
        unreachable!()
    };
    let HeapRhs::Thunk { captures, .. } = &mut caf.binding.rhs else {
        unreachable!()
    };
    captures.push(ValueRef::Local(ValueId(1)));
    wire.expressions
        .nodes
        .push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
            ValueId(0),
        ))]));
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("Failure", "referenceEntry"),
        binding: HeapBinding {
            id: ValueId(2),
            rhs: HeapRhs::Function {
                signature: entry_signature,
                parameters: vec![],
                captures: vec![],
                body: 1,
            },
        },
    }));
    wire
}

#[derive(Clone, Copy)]
enum BottomCall {
    Exact,
    Partial,
    Oversaturated,
    VoidPrefix,
    UnusedReturningJoin,
}

fn bottoming_wire(call: BottomCall) -> WireProgram {
    let bottom_arguments = if matches!(call, BottomCall::VoidPrefix) {
        vec![RuntimeRep::Void, RuntimeRep::Int(64)]
    } else {
        vec![RuntimeRep::Int(64)]
    };
    let call_signature = match call {
        BottomCall::Partial => Signature {
            arguments: Vec::new(),
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
        BottomCall::Oversaturated => Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
        BottomCall::VoidPrefix | BottomCall::Exact | BottomCall::UnusedReturningJoin => Signature {
            arguments: bottom_arguments.clone(),
            results: ResultContract::NoSuccess,
        },
    };
    let mut signatures = vec![
        Signature {
            arguments: Vec::new(),
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
        Signature {
            arguments: bottom_arguments.clone(),
            results: ResultContract::NoSuccess,
        },
        call_signature,
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::NoSuccess,
        },
    ];
    let join_signature = Signature {
        arguments: Vec::new(),
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    };
    let join_signature_id = SignatureId(signatures.len() as u32);
    signatures.push(join_signature);
    let dynamic_call_signature_id = SignatureId(signatures.len() as u32);
    signatures.push(Signature {
        arguments: vec![RuntimeRep::Int(64)],
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    });

    let mut wire = testing::wire_program();
    wire.signatures = signatures;
    wire.operations.push(OperationDecl {
        identity: OperationIdentity::PrimOp("raise#".into()),
        signature: SignatureId(3),
    });
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("Bottoming", "Exception"),
        host_id: tidepool_repr::DataConId(972),
        family: testing::identity("Bottoming", "ExceptionType"),
        result_rep: RuntimeRep::LiftedRef,
        field_reps: vec![],
        strict_fields: vec![],
        layout: CheckedLayout {
            fields: vec![],
            alignment: 1,
            payload_size: 0,
            root_mask: vec![],
        },
        tag: 1,
        family_size: 1,
    });

    let mut nodes = vec![ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
    }];
    let bottom_body = if matches!(call, BottomCall::UnusedReturningJoin) {
        nodes.push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
            ValueId(1),
        ))]));
        nodes.push(ExprFrame::LetJoins {
            bindings: Group::Recursive(vec![JoinBinding {
                id: JoinId(0),
                signature: join_signature_id,
                parameters: vec![],
                body: 1,
            }]),
            body: 0,
        });
        2
    } else {
        0
    };
    let entry_body = match call {
        BottomCall::Partial => {
            nodes.push(ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Local(ValueId(2))),
                signature: SignatureId(2),
                arguments: vec![],
            });
            nodes.push(ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Local(ValueId(20))),
                signature: dynamic_call_signature_id,
                arguments: vec![Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 7_i64.to_be_bytes().to_vec(),
                })],
            });
            nodes.push(ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(20),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                kind: CaseKind::Polymorphic,
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 2,
                }],
            });
            3
        }
        BottomCall::Oversaturated => {
            nodes.push(ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Local(ValueId(2))),
                signature: SignatureId(2),
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
            });
            1
        }
        BottomCall::VoidPrefix => {
            nodes.push(ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Local(ValueId(2))),
                signature: SignatureId(1),
                arguments: vec![
                    Atom::Void,
                    Atom::Scalar(ScalarLiteral::Int {
                        bits: 64,
                        bytes: 7_i64.to_be_bytes().to_vec(),
                    }),
                ],
            });
            1
        }
        BottomCall::Exact | BottomCall::UnusedReturningJoin => {
            nodes.push(ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Local(ValueId(2))),
                signature: SignatureId(1),
                arguments: vec![Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 7_i64.to_be_bytes().to_vec(),
                })],
            });
            nodes.len() - 1
        }
    };
    wire.expressions.nodes = nodes;
    let bottom_parameters = if matches!(call, BottomCall::VoidPrefix) {
        vec![ValueId(10), ValueId(11)]
    } else {
        vec![ValueId(10)]
    };
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("Bottoming", "exception"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }),
        Group::Recursive(vec![
            TopBinding {
                identity: testing::identity("Bottoming", "entry"),
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
                identity: testing::identity("Bottoming", "bottom"),
                binding: HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: bottom_parameters,
                        captures: vec![],
                        body: bottom_body,
                    },
                },
            },
        ]),
    ];
    wire
}

fn compile(wire: WireProgram) -> CompiledProgram {
    let prepared = testing::prepare(wire).expect("NoSuccess fixture validates");
    let linked = link_program(prepared, &MachineImports::default()).expect("fixture links");
    CompiledProgram::compile(&linked).expect("fixture compiles")
}

fn assert_raised(program: &CompiledProgram) {
    let result = program.run_entry(
        ValueId(0),
        &[],
        &RunOptions::default(),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(matches!(
        result,
        Err(ExecutionError::Runtime(MachineFailure {
            cause: RuntimeError::RaisedException,
            disposition: MachineDisposition::Reusable,
        }))
    ));
}

#[test]
fn w5_no_success_raised_caf_uses_status_only_body_and_reusable_settlement() {
    let linked = link_program(
        testing::prepare(raised_caf()).unwrap(),
        &MachineImports::default(),
    )
    .unwrap();
    let program = CompiledProgram::compile(&linked).unwrap();
    for _ in 0..2 {
        let result = program.run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(matches!(
            result,
            Err(ExecutionError::Runtime(MachineFailure {
                cause: RuntimeError::RaisedException,
                disposition: MachineDisposition::Reusable,
            }))
        ));
    }
}

#[test]
fn w5_no_success_raised_caf_retries_in_one_reusable_invocation() {
    let program = compile(raised_caf_reference_entry());
    let mut invocation = super::invocation::PreparedInvocation::enter(
        &program,
        ValueId(2),
        &[],
        &RunOptions {
            nursery_bytes: 256,
            ..RunOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let caf_slot = program.top_slots[&ValueId(0)];
    let exception_slot = program.top_slots[&ValueId(1)];
    let original = untag(invocation.top_table.snapshot()[caf_slot] as usize);
    invocation.collect(0).unwrap();
    let descriptor = program
        .descriptor_registry
        .values()
        .find_map(|metadata| match metadata.meaning {
            DescriptorMeaning::Callable {
                binding: ValueId(0),
                ..
            } => Some(&metadata.descriptor),
            _ => None,
        })
        .unwrap();
    let assert_live = |invocation: &super::invocation::PreparedInvocation<'_>| {
        let tops = invocation.top_table.snapshot();
        let caf = untag(tops[caf_slot] as usize);
        assert_ne!(caf, original, "the rooted CAF must have relocated");
        assert_eq!(
            unsafe { descriptor.state(caf as *const u8, descriptor.allocation_extent() as usize) }
                .unwrap(),
            DescriptorState::Live
        );
        assert_eq!(descriptor.trace_offsets().len(), 1);
        let captured =
            unsafe { ((caf + descriptor.trace_offsets()[0] as usize) as *const usize).read() };
        assert_eq!(untag(captured), untag(tops[exception_slot] as usize));
    };

    for _ in 0..2 {
        let failure = invocation.observe(10_000).unwrap_err();
        assert!(matches!(
            failure,
            ExecutionError::Runtime(MachineFailure {
                cause: RuntimeError::RaisedException,
                disposition: MachineDisposition::Reusable,
            })
        ));
        assert_live(&invocation);
        assert_eq!(
            invocation.machine.take_runtime_error(),
            Some(RuntimeError::RaisedException)
        );
    }
}

#[test]
fn w5_no_success_exact_function_reaches_adapter_terminal() {
    assert_raised(&compile(bottoming_wire(BottomCall::Exact)));
}

#[test]
fn w5_no_success_partial_pap_forces_through_adapter_terminal() {
    assert_raised(&compile(bottoming_wire(BottomCall::Partial)));
}

#[test]
fn w5_no_success_oversaturation_stops_at_saturated_prefix() {
    assert_raised(&compile(bottoming_wire(BottomCall::Oversaturated)));
}

#[test]
fn w5_no_success_void_prefix_uses_logical_arity_without_payload() {
    assert_raised(&compile(bottoming_wire(BottomCall::VoidPrefix)));
}

#[test]
fn w5_no_success_parent_may_contain_an_unused_returning_join() {
    assert_raised(&compile(bottoming_wire(BottomCall::UnusedReturningJoin)));
}
