use super::{
    CompiledProgram, DescriptorMeaning, ExecutionError, ImportBindings, PreparedCallOptions,
    PreparedMachine, PreparedMachineOptions, RunOptions, TopSlotBase,
};
use crate::entry_abi::{AbiError, EntryAbi, EnvironmentMode, NativeAbiProfile};
use crate::host_fns::RuntimeError;
use crate::machine_state::{MachineDisposition, MachineFailure};
use crate::suspension::RealmId;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_bridge::Value;
use tidepool_repr::execution_schema::{testing, *};
use tidepool_repr::{DataConId, Literal};

fn local(id: u32) -> Atom {
    Atom::Ref(ValueRef::Local(ValueId(id)))
}

fn signature(arguments: Vec<RuntimeRep>, results: ResultContract) -> Signature {
    Signature { arguments, results }
}

fn returns(rep: RuntimeRep) -> ResultContract {
    ResultContract::Returns(vec![rep])
}

fn node(wire: &mut WireProgram, expression: ExprFrame<usize>) -> usize {
    let index = wire.expressions.nodes.len();
    wire.expressions.nodes.push(expression);
    index
}

fn function(
    id: u32,
    name: &str,
    signature: u32,
    parameters: Vec<ValueId>,
    body: usize,
) -> Group<TopBinding> {
    Group::NonRecursive(TopBinding {
        identity: testing::identity("CallerResult", name),
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

fn constructor() -> ConstructorDecl {
    ConstructorDecl {
        identity: testing::identity("CallerResult", "Answer"),
        family: testing::identity("CallerResult", "AnswerType"),
        host_id: DataConId(1980),
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
    }
}

fn scalar(rep: RuntimeRep) -> Atom {
    Atom::Scalar(match rep {
        RuntimeRep::Int(64) => ScalarLiteral::Int {
            bits: 64,
            bytes: 42_i64.to_be_bytes().to_vec(),
        },
        RuntimeRep::Word(64) => ScalarLiteral::Word {
            bits: 64,
            bytes: 42_u64.to_be_bytes().to_vec(),
        },
        _ => panic!("fixture scalar representation"),
    })
}

// One generic callback forwarder, with independent concrete host entries. A
// trailing Void parameter makes partial application retain its logical arity.
fn forwarder(use_join: bool, pap: bool) -> WireProgram {
    let lifted = RuntimeRep::LiftedRef;
    let int = RuntimeRep::Int(64);
    let generic_args = if pap {
        vec![lifted, RuntimeRep::Void]
    } else {
        vec![lifted]
    };
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        signature(vec![], returns(int)),
        signature(generic_args.clone(), ResultContract::CallerResult),
        signature(vec![], ResultContract::CallerResult),
        signature(generic_args.clone(), returns(int)),
        signature(vec![], returns(lifted)),
        signature(generic_args, returns(lifted)),
        signature(vec![lifted], returns(lifted)),
        signature(vec![RuntimeRep::Void], returns(int)),
        signature(vec![RuntimeRep::Void], returns(lifted)),
        signature(vec![lifted], ResultContract::CallerResult),
    ];
    wire.constructors = vec![constructor()];
    wire.bindings.clear();
    wire.expressions.nodes.clear();
    let int_body = node(&mut wire, ExprFrame::Return(vec![scalar(int)]));
    wire.bindings
        .push(function(2, "integerCallback", 0, vec![], int_body));
    let lifted_body = node(
        &mut wire,
        ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        },
    );
    wire.bindings
        .push(function(5, "liftedCallback", 4, vec![], lifted_body));
    let mut generic_body = node(
        &mut wire,
        ExprFrame::Call {
            callee: local(if use_join { 31 } else { 3 }),
            signature: SignatureId(2),
            arguments: vec![],
        },
    );
    if use_join {
        let jump = node(
            &mut wire,
            ExprFrame::Jump {
                join: JoinId(0),
                arguments: vec![local(3)],
            },
        );
        generic_body = node(
            &mut wire,
            ExprFrame::LetJoins {
                bindings: Group::NonRecursive(JoinBinding {
                    id: JoinId(0),
                    signature: SignatureId(9),
                    parameters: vec![ValueId(31)],
                    body: generic_body,
                }),
                body: jump,
            },
        );
    }
    let parameters = if pap {
        vec![ValueId(3), ValueId(4)]
    } else {
        vec![ValueId(3)]
    };
    wire.bindings
        .push(function(1, "forward", 1, parameters, generic_body));
    for (entry, callback, full_signature, completion_signature, result, binder) in
        [(0, 2, 3, 7, int, 40), (10, 5, 5, 8, lifted, 50)]
    {
        let body = if pap {
            let partial = node(
                &mut wire,
                ExprFrame::Call {
                    callee: local(1),
                    signature: SignatureId(6),
                    arguments: vec![local(callback)],
                },
            );
            let complete = node(
                &mut wire,
                ExprFrame::Call {
                    callee: local(binder),
                    signature: SignatureId(completion_signature),
                    arguments: vec![Atom::Void],
                },
            );
            node(
                &mut wire,
                ExprFrame::Case {
                    scrutinee: partial,
                    binder: ValueId(binder + 1),
                    scrutinee_results: returns(lifted),
                    kind: CaseKind::MultiValue,
                    alternatives: vec![Alternative {
                        pattern: AlternativePattern::Default,
                        binders: vec![ValueId(binder)],
                        body: complete,
                    }],
                },
            )
        } else {
            node(
                &mut wire,
                ExprFrame::Call {
                    callee: local(1),
                    signature: SignatureId(full_signature),
                    arguments: vec![local(callback)],
                },
            )
        };
        wire.bindings.push(function(
            entry,
            if entry == 0 {
                "entryInt"
            } else {
                "entryLifted"
            },
            if result == int { 0 } else { 4 },
            vec![],
            body,
        ));
    }
    wire
}

fn compile(wire: WireProgram) -> CompiledProgram {
    let linked = link_program(
        testing::prepare(wire).expect("CallerResult fixture validates"),
        &MachineImports::default(),
    )
    .unwrap();
    CompiledProgram::compile(&linked, TopSlotBase::ZERO).expect("CallerResult fixture compiles")
}

fn assert_answer(values: &[Value], rep: RuntimeRep) {
    match rep {
        RuntimeRep::LiftedRef => {
            assert!(matches!(values, [Value::Con(DataConId(1980), fields)] if fields.is_empty()))
        }
        RuntimeRep::Int(64) => assert!(matches!(values, [Value::Lit(Literal::LitInt(42))])),
        RuntimeRep::Word(64) => assert!(matches!(values, [Value::Lit(Literal::LitWord(42))])),
        _ => panic!("fixture answer representation"),
    }
}

#[test]
fn generic_functions_and_joins_forward_lifted_and_unboxed_results() {
    for use_join in [false, true] {
        let program = compile(forwarder(use_join, false));
        assert_eq!(
            program
                .descriptor_registry
                .values()
                .filter(|metadata| matches!(
                    metadata.meaning,
                    DescriptorMeaning::Callable {
                        binding: ValueId(1)
                    }
                ))
                .count(),
            1
        );
        for (entry, rep) in [(0, RuntimeRep::Int(64)), (10, RuntimeRep::LiftedRef)] {
            let result = program
                .run_entry(
                    ValueId(entry),
                    &[],
                    &RunOptions::default(),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
            assert_answer(&result.values, rep);
        }
        assert!(matches!(
            program.run_entry(
                ValueId(1),
                &[],
                &RunOptions::default(),
                Arc::new(AtomicBool::new(false))
            ),
            Err(ExecutionError::MissingEntry(ValueId(1)))
        ));
    }
}

#[test]
fn caller_result_pap_completes_at_both_concrete_result_demands() {
    let program = compile(forwarder(false, true));
    for (entry, rep) in [(0, RuntimeRep::Int(64)), (10, RuntimeRep::LiftedRef)] {
        let result = program
            .run_entry(
                ValueId(entry),
                &[],
                &RunOptions {
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_answer(&result.values, rep);
    }
}

fn consumer(rep: RuntimeRep) -> (WireProgram, MachineImports) {
    let identity = testing::identity("CallerResult", "forward");
    let generic = signature(vec![RuntimeRep::LiftedRef], ResultContract::CallerResult);
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        signature(vec![], returns(rep)),
        generic.clone(),
        signature(vec![RuntimeRep::LiftedRef], returns(rep)),
    ];
    wire.globals.push(GlobalDecl {
        identity: identity.clone(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: Some(SignatureId(1)),
        required_evaluated: true,
        required_generation: None,
    });
    wire.expressions.nodes[0] = if rep == RuntimeRep::LiftedRef {
        wire.constructors.push(constructor());
        ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        }
    } else {
        ExprFrame::Return(vec![scalar(rep)])
    };
    wire.bindings = vec![function(1, "consumerCallback", 0, vec![], 0)];
    let body = node(
        &mut wire,
        ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
            signature: SignatureId(2),
            arguments: vec![local(1)],
        },
    );
    wire.bindings
        .push(function(0, "consumerEntry", 0, vec![], body));
    let mut imports = MachineImports::default();
    imports.values.insert(
        identity.clone(),
        ImportedValue {
            identity,
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(generic),
            evaluated: true,
            generation: 0,
        },
    );
    (wire, imports)
}

#[test]
fn cross_program_generic_calls_offer_concrete_results_and_missing_demand_is_reusable() {
    let (mut machine, producer) = PreparedMachine::new(
        compile(forwarder(false, false)),
        PreparedMachineOptions {
            nursery_bytes: 4096,
            top_slots: 32,
        },
    )
    .unwrap();
    let handle = machine.retain_top(producer, ValueId(1)).unwrap();
    let options = PreparedCallOptions {
        observation_budget: 100,
        collect_before_observation: true,
    };
    for rep in [
        RuntimeRep::Int(64),
        RuntimeRep::LiftedRef,
        RuntimeRep::Word(64),
    ] {
        let (wire, imports) = consumer(rep);
        let linked = link_program(testing::prepare(wire).unwrap(), &imports).unwrap();
        let compiled = machine.compile_for_install(&linked).unwrap();
        let mut bindings = ImportBindings::new();
        bindings.insert(testing::identity("CallerResult", "forward"), handle);
        let consumer = machine.install_program(compiled, bindings).unwrap();
        let result = machine.run_entry(consumer, ValueId(0), &[], options, RealmId::ROOT);
        if rep == RuntimeRep::Word(64) {
            assert!(matches!(
                result,
                Err(ExecutionError::Runtime(MachineFailure {
                    cause: RuntimeError::UnresolvedCallee,
                    disposition: MachineDisposition::Reusable
                }))
            ));
            assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        } else {
            assert_answer(&result.unwrap().values, rep);
        }
    }
    let result = machine
        .run_entry(producer, ValueId(0), &[], options, RealmId::ROOT)
        .unwrap();
    assert_answer(&result.values, RuntimeRep::Int(64));
    assert!(machine.release(handle));
}

#[test]
fn caller_result_requires_instantiation_at_abi_and_concrete_boundaries() {
    let profile = NativeAbiProfile::new(testing::target(), 2).unwrap();
    assert!(matches!(
        EntryAbi::lower(
            &profile,
            &signature(vec![], ResultContract::CallerResult),
            EnvironmentMode::Absent
        ),
        Err(AbiError::UninstantiatedResult)
    ));
    let mut fixed_body = forwarder(false, false);
    let body = fixed_body
        .expressions
        .nodes
        .iter_mut()
        .find(|node| {
            matches!(
                node,
                ExprFrame::Call {
                    callee: Atom::Ref(ValueRef::Local(ValueId(3))),
                    ..
                }
            )
        })
        .unwrap();
    *body = ExprFrame::Return(vec![scalar(RuntimeRep::Int(64))]);
    assert!(matches!(
        testing::prepare(fixed_body),
        Err(ParseError::InvalidSignature(_))
    ));

    let mut entry = forwarder(false, false);
    entry.entry = ValueId(1);
    assert!(matches!(
        testing::prepare(entry),
        Err(ParseError::InvalidSignature(_))
    ));

    let mut thunk = forwarder(false, false);
    thunk
        .signatures
        .push(signature(vec![], ResultContract::CallerResult));
    let index = (thunk.signatures.len() - 1) as u32;
    let body = node(
        &mut thunk,
        ExprFrame::Return(vec![scalar(RuntimeRep::Int(64))]),
    );
    thunk.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("CallerResult", "invalidThunk"),
        binding: HeapBinding {
            id: ValueId(90),
            rhs: HeapRhs::Thunk {
                signature: SignatureId(index),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body,
            },
        },
    }));
    assert!(matches!(
        testing::prepare(thunk),
        Err(ParseError::InvalidSignature(_))
    ));

    let mut operation = testing::wire_program();
    operation
        .signatures
        .push(signature(vec![], ResultContract::CallerResult));
    operation.operations.push(OperationDecl {
        identity: OperationIdentity::PrimOp("raiseDivZero#".into()),
        signature: SignatureId(1),
    });
    operation.expressions.nodes[0] = ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![],
    };
    assert!(matches!(
        testing::prepare(operation),
        Err(ParseError::InvalidSignature(_))
    ));

    let mut scrutinee = forwarder(false, true);
    let case = scrutinee
        .expressions
        .nodes
        .iter_mut()
        .find(|node| matches!(node, ExprFrame::Case { .. }))
        .unwrap();
    let ExprFrame::Case {
        scrutinee_results, ..
    } = case
    else {
        unreachable!()
    };
    *scrutinee_results = ResultContract::CallerResult;
    assert!(matches!(
        testing::prepare(scrutinee),
        Err(ParseError::InvalidSignature(_))
    ));
}
