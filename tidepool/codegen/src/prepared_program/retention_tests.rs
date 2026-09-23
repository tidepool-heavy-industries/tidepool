use super::{CompiledProgram, ExecutionError, RunOptions};
use crate::host_fns::RuntimeError;
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_repr::execution_schema::{self, testing, *};

fn constructor(index: u64, fields: Vec<RuntimeRep>) -> ConstructorDecl {
    let layout = StorageLayout::for_reps(&testing::target(), &fields).unwrap();
    ConstructorDecl {
        identity: testing::identity("W5A5", &format!("C{index}")),
        family: testing::identity("W5A5", &format!("T{index}")),
        host_id: tidepool_repr::DataConId(950 + index),
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

fn compile_wire(wire: WireProgram) -> CompiledProgram {
    let linked =
        execution_schema::link_program(testing::prepare(wire).unwrap(), &MachineImports::default())
            .unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

fn retained_parent_program(garbage: usize) -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(constructor(0, Vec::new()));
    wire.constructors
        .push(constructor(1, vec![RuntimeRep::LiftedRef]));

    wire.expressions.nodes.clear();
    wire.expressions.nodes.push(ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: Vec::new(),
    });
    let mut body = 0;
    for index in 0..garbage {
        wire.expressions.nodes.push(ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(100 + index as u32),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: Vec::new(),
                },
            }),
            body,
        });
        body = wire.expressions.nodes.len() - 1;
    }
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5A5", "parent"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(1),
                    fields: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("W5A5", "child"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: Vec::new(),
                    body,
                },
            },
        }),
    ];
    compile_wire(wire)
}

fn static_constructor_program() -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(constructor(0, Vec::new()));
    wire.expressions.nodes.clear();
    wire.bindings = vec![Group::NonRecursive(TopBinding {
        identity: testing::identity("W5A5", "static"),
        binding: HeapBinding {
            id: ValueId(0),
            rhs: HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: Vec::new(),
            },
        },
    })];
    compile_wire(wire)
}

fn boxed_array_program() -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::UnliftedRef]);
    wire.signatures.push(Signature {
        arguments: vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef, RuntimeRep::Void],
        results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
    });
    wire.constructors.push(constructor(0, Vec::new()));
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("W5A5", "array-initial"),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: Vec::new(),
            },
        },
    }));
    wire.operations.push(OperationDecl {
        identity: OperationIdentity::PrimOp("newSmallArray#".into()),
        signature: SignatureId(1),
    });
    wire.expressions.nodes = vec![
        ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![
                Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 1_i64.to_be_bytes().to_vec(),
                }),
                Atom::Ref(ValueRef::Local(ValueId(1))),
                Atom::Void,
            ],
        },
        ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(4)))]),
        ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(2),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![ValueId(4)],
                body: 1,
            }],
        },
    ];
    if let Group::NonRecursive(top) = &mut wire.bindings[0] {
        if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
            *body = 2;
        }
    }
    compile_wire(wire)
}

fn options() -> RunOptions {
    RunOptions {
        nursery_bytes: 64,
        ..RunOptions::default()
    }
}

fn enter(program: &CompiledProgram) -> super::invocation::PreparedInvocation<'_> {
    let invocation = super::invocation::PreparedInvocation::enter(
        program,
        ValueId(0),
        &[],
        &options(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_scope_clear(&invocation);
    invocation
}

fn collect(invocation: &mut super::invocation::PreparedInvocation<'_>) {
    invocation.collect(0).unwrap();
    assert_scope_clear(invocation);
}

fn assert_scope_clear(invocation: &super::invocation::PreparedInvocation<'_>) {
    assert!(unsafe { invocation.machine.prepared_old_space() }.is_none());
}

#[test]
fn w5_a5_promoted_result_survives_later_minor_collection() {
    let program = retained_parent_program(0);
    let mut invocation = enter(&program);
    invocation.promote_result(0).unwrap();
    assert_scope_clear(&invocation);
    collect(&mut invocation);
    let result = invocation.observe(10_000).unwrap();
    assert_scope_clear(&invocation);
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(951)
                && matches!(fields.as_slice(), [tidepool_bridge::HaskellValue::Con(child, rest)]
                    if *child == tidepool_repr::DataConId(950) && rest.is_empty())
    ));
}

#[test]
fn w5_a5_old_thunk_update_remembers_young_result() {
    let program = retained_parent_program(32);
    let mut invocation = enter(&program);
    invocation.promote_result(0).unwrap();
    assert_scope_clear(&invocation);

    let first = invocation.observe(10_000).unwrap();
    assert_scope_clear(&invocation);
    assert!(matches!(
        first.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(951)
                && matches!(fields.as_slice(), [tidepool_bridge::HaskellValue::Con(child, rest)]
                    if *child == tidepool_repr::DataConId(950) && rest.is_empty())
    ));
    assert!(
        invocation.machine.remembered_slots_count() > 0,
        "forcing a promoted thunk must remember its old-to-young update slot"
    );
    collect(&mut invocation);
    let second = invocation.observe(10_000).unwrap();
    assert_scope_clear(&invocation);
    assert!(matches!(
        second.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(951)
                && matches!(fields.as_slice(), [tidepool_bridge::HaskellValue::Con(child, rest)]
                    if *child == tidepool_repr::DataConId(950) && rest.is_empty())
    ));
}

#[test]
fn w5_a5_promoted_array_remembers_young_store_through_minor_gc() {
    let program = boxed_array_program();
    let mut invocation = enter(&program);
    invocation.promote_result(0).unwrap();
    assert_scope_clear(&invocation);

    let array_descriptor = program
        .descriptors
        .iter()
        .find(|descriptor| {
            descriptor.kind()
                == tidepool_heap::execution_descriptor::ObjectKind::External(
                    tidepool_heap::external_storage::ExternalStorageKind::BoxedArray,
                )
        })
        .unwrap();
    let constructor_descriptor = program
        .descriptors
        .iter()
        .find(|descriptor| {
            descriptor.kind() == tidepool_heap::execution_descriptor::ObjectKind::Constructor
        })
        .unwrap();
    let array = unsafe { *invocation.results.as_mut_ptr().cast::<*mut u8>() };
    let array_object = tidepool_heap::managed_reference::untag(array as usize) as *mut u8;
    let array_extent = array_descriptor.allocation_extent() as usize;
    let payload = unsafe {
        array_descriptor
            .external_payload_slot(array_object, array_extent)
            .unwrap()
            .read()
    };

    // Make a fresh nursery constructor after the array has been promoted.
    // The payload owner is the only root once the store below succeeds.
    let young = invocation.vmctx.alloc_ptr;
    let young_extent = constructor_descriptor.allocation_extent() as usize;
    let nursery_end = invocation.vmctx.alloc_limit as usize;
    assert!((young as usize) + young_extent <= nursery_end);
    unsafe { constructor_descriptor.initialize_header(young) };
    invocation.vmctx.alloc_ptr = unsafe { young.add(young_extent) };
    let young_reference = (young as usize | usize::from(constructor_descriptor.tag())) as *mut u8;

    invocation.machine.clear_remembered_slots();
    invocation
        .machine
        .store_external_element(payload, 0, young_reference)
        .unwrap();
    assert_eq!(invocation.machine.remembered_slots_count(), 1);

    collect(&mut invocation);
    let moved = unsafe { payload.add(8).cast::<*mut u8>().read() };
    assert_ne!(
        moved, young_reference,
        "minor GC must rewrite the retained slot"
    );

    // The ordinary result is the external handle, which is intentionally not
    // directly observable. Re-root the moved managed value through the same
    // invocation result storage, then use the normal observation path.
    unsafe {
        invocation
            .results
            .as_mut_ptr()
            .cast::<*mut u8>()
            .write(moved)
    };
    let result = invocation.observe(10_000).unwrap();
    assert_scope_clear(&invocation);
    assert!(matches!(
        result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(950) && fields.is_empty()
    ));
}

#[test]
fn w5_a5_incomplete_promotion_integrity_failure_is_terminal_and_gated() {
    let program = retained_parent_program(0);
    let mut invocation = enter(&program);
    let result_slot = invocation.results.as_mut_ptr().cast::<*mut u8>();
    let parent =
        unsafe { tidepool_heap::managed_reference::untag(*result_slot as usize) as *mut u8 };
    let header = unsafe { *(parent as *const usize) } & !7;
    let descriptor = program
        .descriptors
        .iter()
        .find(|descriptor| descriptor.initial_header_word() == header)
        .unwrap();
    let field = unsafe {
        parent
            .add(descriptor.trace_offsets()[0] as usize)
            .cast::<*mut u8>()
    };
    // The parent is a valid nursery root, but its selected child is an
    // interior managed pointer. The selected copy forwards the parent before
    // rejecting that edge; the invocation must remain terminal with no retry.
    unsafe { *field = (parent.add(8) as usize | 1) as *mut u8 };

    let error = invocation.promote_result(0).unwrap_err();
    assert_scope_clear(&invocation);
    assert!(matches!(
        error,
        ExecutionError::Runtime(failure)
            if matches!(failure.cause, RuntimeError::IncompletePromotion(_))
    ));
    let observe = invocation.observe(10_000).unwrap_err();
    assert_scope_clear(&invocation);
    assert!(matches!(
        observe,
        ExecutionError::Runtime(failure)
            if matches!(failure.cause, RuntimeError::IncompletePromotion(_))
    ));
    let retry = invocation.promote_result(0).unwrap_err();
    assert_scope_clear(&invocation);
    assert!(matches!(
        retry,
        ExecutionError::Runtime(failure)
            if matches!(failure.cause, RuntimeError::IncompletePromotion(_))
    ));
}

#[test]
fn w5_a5_static_and_already_old_results_are_noops() {
    let static_program = static_constructor_program();
    let mut static_invocation = enter(&static_program);
    static_invocation.promote_result(0).unwrap();
    assert_scope_clear(&static_invocation);
    static_invocation.promote_result(0).unwrap();
    assert_scope_clear(&static_invocation);
    let static_result = static_invocation.observe(10_000).unwrap();
    assert_scope_clear(&static_invocation);
    assert!(matches!(
        static_result.values.as_slice(),
        [tidepool_bridge::HaskellValue::Con(id, fields)]
            if *id == tidepool_repr::DataConId(950) && fields.is_empty()
    ));

    let old_program = retained_parent_program(0);
    let mut old_invocation = enter(&old_program);
    old_invocation.promote_result(0).unwrap();
    assert_scope_clear(&old_invocation);
    let generation = old_invocation.machine.gc_generation();
    old_invocation.promote_result(0).unwrap();
    assert_scope_clear(&old_invocation);
    assert_eq!(old_invocation.machine.gc_generation(), generation);
}

#[test]
fn w5_a5_stable_gate_rejects_forged_outside_references() {
    let program = static_constructor_program();
    for kind in 0..3 {
        let mut invocation = enter(&program);
        let slot = invocation.results.as_mut_ptr().cast::<*mut u8>();
        let static_ref = unsafe { *slot as usize };
        let forged = match kind {
            0 => 0x1000_usize,
            1 => (tidepool_heap::managed_reference::untag(static_ref) + 8) | 1,
            _ => tidepool_heap::managed_reference::untag(static_ref) | 2,
        };
        unsafe { *slot = forged as *mut u8 };
        let error = invocation.promote_result(0).unwrap_err();
        assert_scope_clear(&invocation);
        assert!(matches!(
            error,
            ExecutionError::Runtime(failure) if matches!(failure.cause, RuntimeError::BadPointer { .. })
        ));
    }
}
