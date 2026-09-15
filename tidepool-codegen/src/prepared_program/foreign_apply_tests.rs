//! Foreign application must not depend on the call shapes in the owner's source.

use super::*;
use crate::suspension::RealmId;
use tidepool_repr::execution_schema::{testing, *};

fn local(id: u32) -> Atom {
    Atom::Ref(ValueRef::Local(ValueId(id)))
}

fn top(id: u32, signature: u32, parameters: Vec<ValueId>, body: usize) -> Group<TopBinding> {
    Group::NonRecursive(TopBinding {
        identity: testing::identity("ForeignApply", &format!("f{id}")),
        binding: HeapBinding {
            id: ValueId(id),
            rhs: HeapRhs::Function {
                signature: SignatureId(signature),
                parameters,
                captures: vec![],
                body,
            },
        },
    })
}

fn compile(wire: WireProgram, base: TopSlotBase) -> CompiledProgram {
    let linked = link_program(
        testing::prepare(wire).expect("valid fixture"),
        &MachineImports::default(),
    )
    .unwrap();
    CompiledProgram::compile(&linked, base).expect("fixture compiles")
}

fn install_caller(machine: &mut PreparedMachine<'_>, demand: Signature) -> ProgramId {
    let mut wire = testing::wire_program();
    let mut arguments = vec![RuntimeRep::LiftedRef];
    arguments.extend_from_slice(&demand.arguments);
    wire.signatures = vec![
        Signature {
            arguments,
            results: demand.results.clone(),
        },
        demand.clone(),
    ];
    wire.expressions.nodes = vec![ExprFrame::Call {
        callee: local(100),
        signature: SignatureId(1),
        arguments: demand
            .arguments
            .iter()
            .enumerate()
            .map(|(i, rep)| {
                if *rep == RuntimeRep::Void {
                    Atom::Void
                } else {
                    local(101 + i as u32)
                }
            })
            .collect(),
    }];
    wire.bindings = vec![top(
        0,
        0,
        (0..=demand.arguments.len())
            .map(|i| ValueId(100 + i as u32))
            .collect(),
        0,
    )];
    let program = compile(wire, machine.next_top_slot_base());
    machine
        .install_program(program, ImportBindings::new())
        .unwrap()
}

fn options() -> PreparedCallOptions {
    PreparedCallOptions {
        observation_budget: 0,
        collect_before_observation: false,
    }
}

fn managed(results: PreparedResultBatch) -> PreparedHandle {
    match results.values.as_slice() {
        [PreparedResult::Managed(handle)] => *handle,
        other => panic!("expected managed result, got {other:?}"),
    }
}

fn owner(results: ResultContract) -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures = vec![Signature {
        arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Void, RuntimeRep::Int(64)],
        results: results.clone(),
    }];
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("ForeignApply", "Token"),
        family: testing::identity("ForeignApply", "Token"),
        host_id: DataConId(997),
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
    wire.expressions.nodes = match &results {
        ResultContract::Returns(reps) => vec![ExprFrame::Return(
            reps.iter()
                .map(|rep| match rep {
                    RuntimeRep::LiftedRef => local(100),
                    RuntimeRep::Int(64) => local(102),
                    _ => panic!("fixture result"),
                })
                .collect(),
        )],
        ResultContract::NoSuccess => {
            wire.signatures.push(Signature {
                arguments: vec![RuntimeRep::Void],
                results: ResultContract::NoSuccess,
            });
            wire.operations.push(OperationDecl {
                identity: OperationIdentity::PrimOp("raiseDivZero#".into()),
                signature: SignatureId(1),
            });
            vec![ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![Atom::Void],
            }]
        }
    };
    // Allocate with the incoming managed parameter live; the caller and PAP
    // fields must survive a collection inside the owning program.
    for i in 0..32 {
        wire.expressions.nodes.push(ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(500 + i),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            }),
            body: wire.expressions.nodes.len() - 1,
        });
    }
    wire.bindings = vec![
        top(
            0,
            0,
            vec![ValueId(100), ValueId(101), ValueId(102)],
            wire.expressions.nodes.len() - 1,
        ),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("ForeignApply", "token"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }),
    ];
    wire
}

#[test]
fn foreign_pap_partial_again_and_exact_results_survive_gc() {
    for reps in [
        vec![RuntimeRep::LiftedRef],
        vec![RuntimeRep::Int(64)],
        vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef],
    ] {
        let results = ResultContract::Returns(reps.clone());
        let (mut machine, a) = PreparedMachine::new(
            compile(owner(results.clone()), TopSlotBase::ZERO),
            PreparedMachineOptions {
                nursery_bytes: 128,
                top_slots: 32,
            },
        )
        .unwrap();
        let function = machine.retain_top(a, ValueId(0)).unwrap();
        let token = machine.retain_top(a, ValueId(1)).unwrap();
        let b = install_caller(
            &mut machine,
            Signature {
                arguments: vec![RuntimeRep::LiftedRef],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
        );
        let pap = managed(
            machine
                .run_entry_retained(
                    b,
                    ValueId(0),
                    &[
                        PreparedInput::Managed(function),
                        PreparedInput::Managed(token),
                    ],
                    options(),
                    RealmId::ROOT,
                )
                .unwrap(),
        );
        let c = install_caller(
            &mut machine,
            Signature {
                arguments: vec![RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
        );
        // Void consumes logical arity without adding a physical argument.
        let pap2 = managed(
            machine
                .run_entry_retained(
                    c,
                    ValueId(0),
                    &[PreparedInput::Managed(pap)],
                    options(),
                    RealmId::ROOT,
                )
                .unwrap(),
        );
        let d = install_caller(
            &mut machine,
            Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results,
            },
        );
        let result = machine
            .run_entry_retained(
                d,
                ValueId(0),
                &[PreparedInput::Managed(pap2), PreparedInput::Scalar(42)],
                options(),
                RealmId::ROOT,
            )
            .unwrap();
        assert!(
            result.collections > 0,
            "foreign body must collect with caller/PAP roots live"
        );
        for (rep, value) in reps.iter().zip(result.values) {
            match (rep, value) {
                (RuntimeRep::Int(64), PreparedResult::Scalar(42)) => {}
                (RuntimeRep::LiftedRef, PreparedResult::Managed(handle)) => {
                    assert!(matches!(
                        machine.inspect_outer(handle, RealmId::ROOT).unwrap(),
                        PreparedOuter::Constructor {
                            identity: DataConId(997),
                            ..
                        }
                    ));
                    assert!(machine.release(handle));
                }
                other => panic!("wrong result: {other:?}"),
            }
        }
        for handle in [function, token, pap, pap2] {
            assert!(machine.release(handle));
        }
        assert_eq!(machine.handle_count(), 0);
    }
}

#[test]
fn foreign_terminal_saturation_does_not_apply_excess_or_publish_results() {
    let (mut machine, a) = PreparedMachine::new(
        compile(owner(ResultContract::NoSuccess), TopSlotBase::ZERO),
        PreparedMachineOptions {
            nursery_bytes: 128,
            top_slots: 32,
        },
    )
    .unwrap();
    let function = machine.retain_top(a, ValueId(0)).unwrap();
    let token = machine.retain_top(a, ValueId(1)).unwrap();
    for excess in [false, true] {
        let mut args = vec![RuntimeRep::LiftedRef, RuntimeRep::Void, RuntimeRep::Int(64)];
        let mut inputs = vec![
            PreparedInput::Managed(function),
            PreparedInput::Managed(token),
            PreparedInput::Scalar(42),
        ];
        if excess {
            args.push(RuntimeRep::Int(64));
            inputs.push(PreparedInput::Scalar(7));
        }
        let b = install_caller(
            &mut machine,
            Signature {
                arguments: args,
                results: ResultContract::Returns(vec![
                    RuntimeRep::Float(64),
                    RuntimeRep::LiftedRef,
                ]),
            },
        );
        let error = machine
            .run_entry_retained(b, ValueId(0), &inputs, options(), RealmId::ROOT)
            .err()
            .expect("terminal call fails");
        assert!(matches!(
            error,
            ExecutionError::Runtime(crate::machine_state::MachineFailure {
                cause: crate::host_fns::RuntimeError::DivisionByZero,
                disposition: crate::machine_state::MachineDisposition::Reusable
            })
        ));
    }
    let partial = install_caller(
        &mut machine,
        Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
    );
    let pap = managed(
        machine
            .run_entry_retained(
                partial,
                ValueId(0),
                &[
                    PreparedInput::Managed(function),
                    PreparedInput::Managed(token),
                ],
                options(),
                RealmId::ROOT,
            )
            .unwrap(),
    );
    let complete = install_caller(
        &mut machine,
        Signature {
            arguments: vec![RuntimeRep::Void, RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        },
    );
    assert!(matches!(
        machine.run_entry_retained(
            complete,
            ValueId(0),
            &[PreparedInput::Managed(pap), PreparedInput::Scalar(42)],
            options(),
            RealmId::ROOT
        ),
        Err(ExecutionError::Runtime(
            crate::machine_state::MachineFailure {
                cause: crate::host_fns::RuntimeError::DivisionByZero,
                disposition: crate::machine_state::MachineDisposition::Reusable,
            }
        ))
    ));
    for handle in [pap, function, token] {
        assert!(machine.release(handle));
    }
}

#[test]
fn foreign_excess_probe_misses_do_not_poison_a_later_hit() {
    let mut wire = owner(ResultContract::Returns(vec![
        RuntimeRep::Int(64),
        RuntimeRep::LiftedRef,
    ]));
    // Entry returns a closure capturing its managed argument. Only the caller
    // over-applies it; the owner contains no source call at that demand.
    wire.signatures[0] = Signature {
        arguments: vec![RuntimeRep::LiftedRef],
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    };
    wire.signatures.push(Signature {
        arguments: vec![RuntimeRep::Int(64)],
        results: ResultContract::Returns(vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef]),
    });
    let inner_body = wire.expressions.nodes.len() - 1;
    wire.expressions
        .nodes
        .push(ExprFrame::Return(vec![local(200)]));
    wire.expressions.nodes.push(ExprFrame::Let {
        bindings: Group::NonRecursive(HeapBinding {
            id: ValueId(200),
            rhs: HeapRhs::Function {
                signature: SignatureId(1),
                parameters: vec![ValueId(102)],
                captures: vec![ValueRef::Local(ValueId(100))],
                body: inner_body,
            },
        }),
        body: wire.expressions.nodes.len() - 1,
    });
    wire.bindings[0] = top(0, 0, vec![ValueId(100)], wire.expressions.nodes.len() - 1);
    let (mut machine, a) = PreparedMachine::new(
        compile(wire, TopSlotBase::ZERO),
        PreparedMachineOptions {
            nursery_bytes: 128,
            top_slots: 32,
        },
    )
    .unwrap();
    let function = machine.retain_top(a, ValueId(0)).unwrap();
    let token = machine.retain_top(a, ValueId(1)).unwrap();
    let b = install_caller(
        &mut machine,
        Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef]),
        },
    );
    let result = machine
        .run_entry_retained(
            b,
            ValueId(0),
            &[
                PreparedInput::Managed(function),
                PreparedInput::Managed(token),
                PreparedInput::Scalar(42),
            ],
            options(),
            RealmId::ROOT,
        )
        .unwrap();
    assert!(result.collections > 0);
    assert!(matches!(
        result.values.as_slice(),
        [PreparedResult::Scalar(42), PreparedResult::Managed(_)]
    ));
    assert_eq!(
        machine.disposition(),
        crate::machine_state::MachineDisposition::Reusable
    );
    for result in result.values {
        if let PreparedResult::Managed(handle) = result {
            assert!(machine.release(handle));
        }
    }
    for handle in [function, token] {
        assert!(machine.release(handle));
    }
}

#[test]
fn foreign_signature_matching_preserves_void_positions_and_zero_application() {
    let (mut machine, a) = PreparedMachine::new(
        compile(
            owner(ResultContract::Returns(vec![RuntimeRep::Int(64)])),
            TopSlotBase::ZERO,
        ),
        PreparedMachineOptions {
            nursery_bytes: 128,
            top_slots: 32,
        },
    )
    .unwrap();
    let function = machine.retain_top(a, ValueId(0)).unwrap();
    let token = machine.retain_top(a, ValueId(1)).unwrap();
    let empty = install_caller(
        &mut machine,
        Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        },
    );
    let same_function = managed(
        machine
            .run_entry_retained(
                empty,
                ValueId(0),
                &[PreparedInput::Managed(function)],
                options(),
                RealmId::ROOT,
            )
            .unwrap(),
    );
    // These demands have identical physical register shapes but different
    // logical Void positions. A native-signature-only key would accept both.
    let wrong = install_caller(
        &mut machine,
        Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64), RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        },
    );
    let inputs = [
        PreparedInput::Managed(same_function),
        PreparedInput::Managed(token),
        PreparedInput::Scalar(42),
    ];
    assert!(matches!(
        machine.run_entry_retained(wrong, ValueId(0), &inputs, options(), RealmId::ROOT),
        Err(ExecutionError::Runtime(
            crate::machine_state::MachineFailure {
                cause: crate::host_fns::RuntimeError::UnresolvedCallee,
                disposition: crate::machine_state::MachineDisposition::Reusable,
            }
        ))
    ));
    let right = install_caller(
        &mut machine,
        Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Void, RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        },
    );
    let result = machine
        .run_entry_retained(right, ValueId(0), &inputs, options(), RealmId::ROOT)
        .unwrap();
    assert!(matches!(
        result.values.as_slice(),
        [PreparedResult::Scalar(42)]
    ));
    for handle in [function, same_function, token] {
        assert!(machine.release(handle));
    }
}

#[test]
fn foreign_excess_can_continue_in_a_third_program() {
    let mut identity = testing::wire_program();
    identity.signatures[0] = Signature {
        arguments: vec![RuntimeRep::LiftedRef],
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    };
    identity.expressions.nodes = vec![ExprFrame::Return(vec![local(100)])];
    identity.bindings = vec![top(0, 0, vec![ValueId(100)], 0)];
    let (mut machine, a) = PreparedMachine::new(
        compile(identity, TopSlotBase::ZERO),
        PreparedMachineOptions {
            nursery_bytes: 128,
            top_slots: 32,
        },
    )
    .unwrap();
    let third = compile(
        owner(ResultContract::Returns(vec![
            RuntimeRep::Int(64),
            RuntimeRep::LiftedRef,
        ])),
        machine.next_top_slot_base(),
    );
    let c = machine
        .install_program(third, ImportBindings::new())
        .unwrap();
    let identity = machine.retain_top(a, ValueId(0)).unwrap();
    let function = machine.retain_top(c, ValueId(0)).unwrap();
    let token = machine.retain_top(c, ValueId(1)).unwrap();
    let b = install_caller(
        &mut machine,
        Signature {
            arguments: vec![
                RuntimeRep::LiftedRef,
                RuntimeRep::LiftedRef,
                RuntimeRep::Void,
                RuntimeRep::Int(64),
            ],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef]),
        },
    );
    let result = machine
        .run_entry_retained(
            b,
            ValueId(0),
            &[
                PreparedInput::Managed(identity),
                PreparedInput::Managed(function),
                PreparedInput::Managed(token),
                PreparedInput::Scalar(42),
            ],
            options(),
            RealmId::ROOT,
        )
        .unwrap();
    assert!(result.collections > 0);
    assert!(matches!(
        result.values.as_slice(),
        [PreparedResult::Scalar(42), PreparedResult::Managed(_)]
    ));
    for value in result.values {
        if let PreparedResult::Managed(handle) = value {
            assert!(machine.release(handle));
        }
    }
    for handle in [identity, function, token] {
        assert!(machine.release(handle));
    }
    assert_eq!(machine.handle_count(), 0);
}

#[test]
#[ignore = "manual compilation cost observation; run with --ignored --nocapture"]
fn foreign_dispatch_cost_on_freer_artifact() {
    let bytes = include_bytes!("../../../haskell/test-prepared-stg/fixtures/freer-resume.cbor");
    let envelope = testing::envelope();
    let requirements = ProgramRequirements {
        schema_version: envelope.schema_version,
        projection_profile: envelope.projection_profile,
        toolchain: envelope.toolchain,
        execution_abi_version: envelope.execution_abi_version,
        target: envelope.target,
    };
    let prepared = parse_program(bytes, &requirements, DecodeLimits::default()).unwrap();
    let linked = link_program(prepared, &MachineImports::default()).unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let started = std::time::Instant::now();
        let program = CompiledProgram::compile(&linked, TopSlotBase::ZERO).unwrap();
        eprintln!(
            "freer-resume: {} callable offers, total compile {:?}",
            program.callables.len(),
            started.elapsed()
        );
    });
}
