use super::{CompiledProgram, RunOptions};
use crate::host_fns::RuntimeError;
use std::collections::BTreeMap;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_heap::static_region::{StaticImage, StaticRelocation};
use tidepool_repr::execution_schema::{testing, *};

#[test]
fn w5_a4_observation_forces_constructor_child() {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    for (index, fields) in [vec![RuntimeRep::LiftedRef], vec![]]
        .into_iter()
        .enumerate()
    {
        let layout = StorageLayout::for_reps(&wire.envelope.target, &fields).unwrap();
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("W5", &format!("C{index}")),
            family: testing::identity("W5", &format!("T{index}")),
            host_id: tidepool_repr::DataConId(920 + index as u64),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            strict_fields: vec![false; fields.len()],
            field_reps: fields,
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
                root_mask: layout
                    .fields()
                    .iter()
                    .map(|field| {
                        matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)
                    })
                    .collect(),
            },
        });
    }
    wire.expressions.nodes[0] = ExprFrame::Construct {
        constructor: ConstructorId(1),
        fields: vec![],
    };
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "child"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: 0,
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "parent"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
                },
            },
        }),
    ];
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    let compiled = CompiledProgram::compile(&linked).unwrap();
    let result = compiled
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(matches!(result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(tidepool_repr::DataConId(920), fields)]
        if matches!(fields.as_slice(),
            [tidepool_bridge::HaskellValue::Con(tidepool_repr::DataConId(921), children)] if children.is_empty())));
}

pub(super) fn caf_program(
    garbage_objects: u32,
    local_thunk: bool,
    update: UpdatePolicy,
) -> CompiledProgram {
    CompiledProgram::compile(&caf_linked(garbage_objects, local_thunk, update)).unwrap()
}

/// [`caf_program`]'s linked program, for installs that compile against an
/// existing machine's interner.
pub(super) fn caf_linked(
    garbage_objects: u32,
    local_thunk: bool,
    update: UpdatePolicy,
) -> tidepool_repr::execution_schema::LinkedProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
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
            alignment: 1,
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
    if local_thunk {
        let thunk_id = ValueId(10_000);
        let enter = wire.expressions.nodes.len();
        wire.expressions.nodes.push(ExprFrame::Enter {
            callee: Atom::Ref(ValueRef::Local(thunk_id)),
            signature: SignatureId(0),
        });
        wire.expressions.nodes.push(ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: thunk_id,
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: 0,
                },
            }),
            body: enter,
        });
        body = wire.expressions.nodes.len() - 1;
    }
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        unreachable!()
    };
    top.binding.rhs = HeapRhs::Thunk {
        signature: SignatureId(0),
        update,
        captures: vec![],
        body,
    };
    let prepared = testing::prepare(wire).unwrap();
    link_program(prepared, &MachineImports::default()).unwrap()
}

/// The real adapter must enter a heap CAF, allocate its value and settle it.
#[test]
fn w5_a1_top_thunk_constructs_and_observes() {
    let program = caf_program(0, false, UpdatePolicy::Memoize);
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
        matches!(&result.values[0], tidepool_bridge::HaskellValue::Con(id, fields) if *id == tidepool_repr::DataConId(900) && fields.is_empty())
    );
}

#[test]
fn w5_a1_collection_during_thunk_body_keeps_update_root_live() {
    let program = caf_program(32, false, UpdatePolicy::Memoize);
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
        matches!(&result.values[0], tidepool_bridge::HaskellValue::Con(id, fields)
        if *id == tidepool_repr::DataConId(900) && fields.is_empty())
    );
}

#[test]
fn w5_a1_local_thunk_enter_routes_prepared_entry() {
    let program = caf_program(0, true, UpdatePolicy::Memoize);
    let result = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(matches!(
        &result.values[0],
        tidepool_bridge::HaskellValue::Con(id, fields)
            if *id == tidepool_repr::DataConId(900) && fields.is_empty()
    ));
}

#[test]
fn w5_a1_single_entry_success_is_observable_after_heap_scan() {
    let program = caf_program(0, false, UpdatePolicy::SingleEntry);
    let result = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(matches!(
        &result.values[0],
        tidepool_bridge::HaskellValue::Con(id, fields)
            if *id == tidepool_repr::DataConId(900) && fields.is_empty()
    ));
}

#[test]
fn w5_a1_function_case_enters_captured_local_thunk_across_collection() {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("W5", "Unit"),
        family: testing::identity("W5", "Unit"),
        host_id: tidepool_repr::DataConId(901),
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
    });
    let captured = ValueId(1);
    let thunk = ValueId(2);
    let binder = ValueId(3);
    wire.expressions.nodes = vec![
        ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(captured))]),
        ExprFrame::Enter {
            callee: Atom::Ref(ValueRef::Local(thunk)),
            signature: SignatureId(0),
        },
        ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(binder))]),
        ExprFrame::Case {
            scrutinee: 1,
            binder,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            kind: CaseKind::Polymorphic,
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body: 2,
            }],
        },
        ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: thunk,
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Local(captured)],
                    body: 0,
                },
            }),
            body: 3,
        },
        ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: captured,
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            }),
            body: 4,
        },
    ];
    let mut body = 5;
    for id in 10..42 {
        wire.expressions.nodes.push(ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(id),
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
    top.binding.rhs = HeapRhs::Function {
        signature: SignatureId(0),
        parameters: vec![],
        captures: vec![],
        body,
    };
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    let program = CompiledProgram::compile(&linked).unwrap();
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
        .unwrap();
    assert!(result.collections > 0);
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(901) && fields.is_empty()
    ));
}

fn compile_wire(wire: WireProgram) -> CompiledProgram {
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

fn constructor_decl(index: u64, fields: Vec<RuntimeRep>) -> ConstructorDecl {
    let layout = StorageLayout::for_reps(&testing::target(), &fields).unwrap();
    ConstructorDecl {
        identity: testing::identity("W5", &format!("C{index}")),
        family: testing::identity("W5", &format!("T{index}")),
        host_id: tidepool_repr::DataConId(940 + index),
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
        strict_fields: vec![false; fields.len()],
        field_reps: fields,
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
            root_mask: layout
                .fields()
                .iter()
                .map(|field| matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef))
                .collect(),
        },
    }
}

#[test]
fn w5_a4_lazy_alias_chain_is_observable() {
    let depth = 96_u64;
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(constructor_decl(0, vec![]));
    wire.expressions.nodes.clear();
    for index in 0..depth {
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
                ValueId((index + 1) as u32),
            ))]));
    }
    wire.bindings.clear();
    wire.bindings.extend((0..=depth).map(|index| {
        let rhs = if index == depth {
            HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: vec![],
            }
        } else {
            HeapRhs::Thunk {
                signature: SignatureId(0),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body: index as usize,
            }
        };
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", &format!("chain{index}")),
            binding: HeapBinding {
                id: ValueId(index as u32),
                rhs,
            },
        })
    }));
    let result = compile_wire(wire)
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 64,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(940) && fields.is_empty()
    ));
}

#[test]
fn w5_a4_function_result_observes_as_the_closure_sentinel() {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors
        .push(constructor_decl(0, vec![RuntimeRep::LiftedRef]));
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
        bits: 64,
        bytes: 0_i64.to_be_bytes().to_vec(),
    })]);
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(0)))]);
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5", "function"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![],
                    captures: vec![],
                    body: 0,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5", "function-parent"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
                },
            },
        },
    ])];
    // A function-valued field observes as the closure sentinel inside its
    // parent constructor, preserving the surrounding data value.
    let result = compile_wire(wire)
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(_, fields)]
            if matches!(
                fields.as_slice(),
                [tidepool_bridge::HaskellValue::Con(id, inner)]
                    if *id == crate::observation::CLOSURE_SENTINEL && inner.is_empty()
            )
    ));
}

#[test]
fn w5_a4_cycle_uses_one_bounded_observation_budget() {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors
        .push(constructor_decl(0, vec![RuntimeRep::LiftedRef]));
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(0)))]);
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5", "cycle"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![Atom::Ref(ValueRef::Local(ValueId(0)))],
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5", "cycle-helper"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![],
                    captures: vec![],
                    body: 0,
                },
            },
        },
    ])];
    let error = compile_wire(wire)
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                observation_budget: 5,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        super::ExecutionError::Observation(super::ObservationFailure::BudgetExceeded { limit: 5 })
    ));
}

#[test]
fn w5_a4_cancelled_run_cleans_observation_roots() {
    let program = caf_program(0, false, UpdatePolicy::Memoize);
    let cancelled = program.run_entry(
        ValueId(0),
        &[],
        &RunOptions::default(),
        Arc::new(AtomicBool::new(true)),
    );
    assert!(matches!(
        cancelled,
        Err(super::ExecutionError::Runtime(failure))
            if failure.cause == RuntimeError::Cancelled
    ));
    let result = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(900) && fields.is_empty()
    ));
}

#[test]
fn w5_a4_child_force_moves_heap_without_losing_sibling_root() {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(constructor_decl(0, vec![]));
    wire.constructors.push(constructor_decl(
        1,
        vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
    ));
    wire.expressions.nodes = vec![ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
        ValueId(2),
    ))])];
    let mut child_body = 0;
    for index in 0..32_u32 {
        wire.expressions.nodes.push(ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(100 + index),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            }),
            body: child_body,
        });
        child_body = wire.expressions.nodes.len() - 1;
    }
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "child-moving"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: child_body,
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "sibling"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "parent-moving"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(1),
                    fields: vec![
                        Atom::Ref(ValueRef::Local(ValueId(1))),
                        Atom::Ref(ValueRef::Local(ValueId(2))),
                    ],
                },
            },
        }),
    ];
    let result = compile_wire(wire)
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 64,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(
        result.collections > 0,
        "child force must move nursery objects"
    );
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(parent, fields)]
            if *parent == tidepool_repr::DataConId(941)
                && matches!(
                    fields.as_slice(),
                    [tidepool_bridge::HaskellValue::Con(left, left_fields),
                     tidepool_bridge::HaskellValue::Con(right, right_fields)]
                        if *left == tidepool_repr::DataConId(940)
                            && left_fields.is_empty()
                            && *right == tidepool_repr::DataConId(940)
                            && right_fields.is_empty()
                )
    ));
}

fn deep_forcing_wire() -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(constructor_decl(0, vec![]));
    wire.constructors
        .push(constructor_decl(1, vec![RuntimeRep::LiftedRef]));
    wire.expressions.nodes = vec![ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
        ValueId(1),
    ))])];
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "deep-lazy-root"),
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
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "deep-static-root"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(1),
                    fields: vec![Atom::Ref(ValueRef::Local(ValueId(2)))],
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5", "deep-static-leaf"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        }),
    ];
    wire.entry = ValueId(0);
    wire
}

fn install_deep_static_chain(program: &mut CompiledProgram, depth: usize) {
    let leaf = Arc::clone(&program.descriptors[0]);
    let node = Arc::clone(&program.descriptors[1]);
    let node_extent = node.allocation_extent() as usize;
    let leaf_offset = depth
        .checked_mul(node_extent)
        .expect("deep static chain extent must fit");
    let total_bytes = leaf_offset
        .checked_add(leaf.allocation_extent() as usize)
        .expect("deep static chain size must fit");
    let mut words = vec![0_u64; total_bytes / std::mem::size_of::<u64>()];
    let mut relocations = Vec::with_capacity(depth);
    for index in 0..depth {
        let offset = index * node_extent;
        unsafe { node.initialize_header(words.as_mut_ptr().cast::<u8>().add(offset)) };
        let field = node.payload().fields().first().expect("node has one field");
        let slot_offset = offset + node.payload_base() as usize + field.offset() as usize;
        let target_offset = if index + 1 == depth {
            leaf_offset
        } else {
            (index + 1) * node_extent
        };
        relocations.push(StaticRelocation {
            slot_offset,
            target_offset,
            tag: if index + 1 == depth {
                leaf.tag()
            } else {
                node.tag()
            },
        });
    }
    unsafe { leaf.initialize_header(words.as_mut_ptr().cast::<u8>().add(leaf_offset)) };
    let entries = BTreeMap::from([(ValueId(1), 0), (ValueId(2), leaf_offset)]);
    program.statics = StaticImage::new(words, relocations, entries, [leaf, node])
        .expect("iteratively built deep static chain must validate");
}

/// The root is lazy, and observation forces it before iteratively expanding
/// 20,000 constructor nodes from a test-private validated static image.
#[test]
fn w5_a4_forcing_observation_20k_constructors_small_stack() {
    if std::env::var_os("TIDEPOOL_A4_DEEP_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "prepared_program::entry_tests::w5_a4_forcing_observation_20k_constructors_small_stack",
                "--nocapture",
            ])
            .env("TIDEPOOL_A4_DEEP_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "small-stack forcing child failed: {output:?}"
        );
        return;
    }

    let wire = deep_forcing_wire();
    let join = std::thread::Builder::new()
        .name("prepared-a4-deep-observe".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut program = compile_wire(wire);
            install_deep_static_chain(&mut program, 20_000);
            let result = program
                .run_entry(
                    ValueId(0),
                    &[],
                    &RunOptions {
                        observation_budget: 20_002,
                        ..RunOptions::default()
                    },
                    Arc::new(AtomicBool::new(false)),
                )
                .expect("deep forcing observation should complete");
            assert_eq!(result.values.len(), 1);
            assert_eq!(result.values[0].node_count(), 20_001);
        })
        .expect("small-stack worker should start")
        .join();
    join.expect("deep forcing observation must not overflow the worker stack");
}

/// Machine-shared descriptors (interned constructors, external wrappers) have
/// no owning program: installing a program that declares one must not make
/// its enter row that program's, and retiring that program must not remove
/// what later programs rely on.
mod shared_constructor_rows {
    use super::super::{
        AnswerPlan, ImportBindings, PreparedCallOptions, PreparedMachine, PreparedMachineOptions,
        RunOptions,
    };
    use super::{caf_linked, caf_program};
    use crate::suspension::RealmId;
    use tidepool_bridge::HaskellValue;
    use tidepool_repr::execution_schema::{
        link_program, testing, MachineImports, UpdatePolicy, ValueId,
    };
    use tidepool_repr::DataConId;

    /// A program that declares no constructor at all: any constructor value it
    /// enters is foreign to its own `prepared_enter` chain.
    fn constructor_free() -> tidepool_repr::execution_schema::LinkedProgram {
        link_program(
            testing::prepare(testing::wire_program()).expect("baseline fixture validates"),
            &MachineImports::default(),
        )
        .expect("baseline fixture links")
    }

    #[test]
    fn prepared_program_retiring_a_declarer_keeps_shared_constructor_entry() {
        let options = PreparedMachineOptions {
            nursery_bytes: RunOptions::default().nursery_bytes,
        };
        // A and B both declare the shared `Unit`; C declares nothing.
        let (mut machine, program_a) =
            PreparedMachine::new(caf_program(0, false, UpdatePolicy::Memoize), options)
                .expect("A installs");
        let compiled_b = machine
            .compile_for_install(&caf_linked(0, false, UpdatePolicy::Memoize))
            .expect("B compiles against the machine interner");
        let program_b = machine
            .install_program(compiled_b, ImportBindings::new())
            .expect("B installs");
        let compiled_c = machine
            .compile_for_install(&constructor_free())
            .expect("C compiles");
        let program_c = machine
            .install_program(compiled_c, ImportBindings::new())
            .expect("C installs");
        machine.pin(program_b).expect("B is installed");
        machine.pin(program_c).expect("C is installed");

        let call = PreparedCallOptions {
            observation_budget: 100,
            collect_before_observation: false,
        };
        machine
            .run_entry(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B builds its Unit");
        let unit = machine
            .build_answer(
                RealmId::ROOT,
                &AnswerPlan::Constructor {
                    host_id: DataConId(900),
                    fields: Vec::new(),
                },
            )
            .expect("a shared Unit builds");
        let before = machine.residency().enter_rows;

        let token = machine.quiesce().expect("quiescent");
        let receipt = machine.collect_major(token).expect("major collection");
        assert_eq!(receipt.programs, vec![program_a], "{receipt:?}");
        assert!(
            machine.residency().enter_rows < before,
            "A's own thunk row leaves with A"
        );

        // Constructor references are normally pointer-tagged, which `prepared_enter`
        // returns without reading the header. The untagged encoding is equally
        // valid and takes the header path, where C has no local row for `Unit`.
        let slot = machine.handle_root(unit).expect("the Unit handle is live");
        unsafe {
            let word = slot.current() as usize;
            assert_ne!(word & 7, 0, "host answers are pointer-tagged");
            slot.addr().write((word & !7) as *mut u8);
        }

        for program in [program_c, program_b] {
            assert!(
                matches!(
                    machine.observe_handle(program, unit, 100),
                    Ok(HaskellValue::Con(id, ref fields)) if id == DataConId(900) && fields.is_empty()
                ),
                "entering the shared constructor through {program:?} after A retired"
            );
        }
        machine
            .run_entry(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B still runs after A retired");
        assert!(machine.release(unit));
    }
}
