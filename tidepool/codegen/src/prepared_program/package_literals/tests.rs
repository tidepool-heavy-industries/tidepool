use super::*;
use crate::prepared_program::{
    BatchImport, BatchProgram, DemandError, DemandedImage, ExecutionError, ImageRegistry,
    PreparedCallOptions, PreparedInput, PreparedMachine, PreparedMachineOptions, PreparedResult,
};
use crate::suspension::RealmId;
use tidepool_repr::execution_schema::{
    testing, Atom, CachedHomeOwner, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame,
    FieldLayout, GlobalDecl, Group, HeapBinding, HeapRhs, ModuleVersion, ResultContract, Signature,
    SignatureId, TopBinding, ValueId, ValueRef, WireProgram,
};

const DIGEST: [u8; 32] = [7; 32];
const BYTES: &[u8] = b"True\0tail";

fn identity() -> SymbolIdentity {
    let mut id = testing::identity("Package", "literal");
    id.unit = "package".into();
    id
}

fn compile_target(bytes: &[u8]) -> Arc<CompiledProgram> {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: identity(),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Bytes(bytes.to_vec()),
        },
    }));
    Arc::new(
        CompiledProgram::compile_prepared_definitions(&testing::prepare(wire).unwrap()).unwrap(),
    )
}

fn tokens(target: &CompiledProgram) -> BTreeMap<SymbolIdentity, PackageLiteral> {
    target.package_literals(|unit, module| {
        (unit == "package" && module == "Package").then_some(DIGEST)
    })
}

fn group(captured: bool, digest: [u8; 32], evaluated: bool) -> CertifiedGroup {
    certify(
        group_wire(captured, evaluated),
        ImportOwner::Package {
            unit: "package".into(),
            module: "Package".into(),
            binder: identity(),
            interface_digest: digest,
        },
    )
}

fn group_wire(captured: bool, evaluated: bool) -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
    if let Group::NonRecursive(top) = &mut wire.bindings[0] {
        if let HeapRhs::Function { captures, .. } = &mut top.binding.rhs {
            if captured {
                captures.push(ValueRef::Global(GlobalId(0)));
            }
        }
    }
    wire.globals.push(GlobalDecl {
        identity: identity(),
        rep: RuntimeRep::Address,
        entry_signature: None,
        required_evaluated: evaluated,
        required_generation: None,
    });
    wire
}

fn certify(wire: WireProgram, owner: ImportOwner) -> CertifiedGroup {
    CertifiedGroup::admit(
        CachedHomeOwner {
            unit: "fixture".into(),
            module: "Fixture".into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: [2; 32],
            product_sha256: [3; 32],
        },
        testing::projected_group(wire, 183).unwrap(),
        vec![owner],
    )
    .unwrap()
}

#[test]
fn package_literal_source_addresses_and_generic_global_captures_stay_refused() {
    let target = compile_target(BYTES);
    let registry = ImageRegistry::new();
    let source = certify(
        group_wire(false, true),
        ImportOwner::Source {
            version: ModuleVersion([4; 32]),
            binder: identity(),
        },
    );
    assert!(matches!(
        DemandedImage::compile_with_package_literals(source, &registry, &tokens(&target)),
        Err(DemandError::Compile(CompileError::Unsupported(_)))
    ));
    let mut wire = group_wire(true, false);
    wire.globals[0].rep = RuntimeRep::LiftedRef;
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    let source = certify(
        wire,
        ImportOwner::Source {
            version: ModuleVersion([4; 32]),
            binder: identity(),
        },
    );
    assert!(matches!(
        DemandedImage::compile_with_package_literals(source, &registry, &tokens(&target)),
        Err(DemandError::Compile(CompileError::Unsupported(_)))
    ));
}

#[test]
fn package_literal_static_constructor_field_is_not_traced() {
    let mut wire = group_wire(false, true);
    wire.expressions.nodes.clear();
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("PackageLiteral", "Holder"),
        family: testing::identity("PackageLiteral", "Holder"),
        host_id: tidepool_repr::DataConId(90_103),
        result_rep: RuntimeRep::LiftedRef,
        field_reps: vec![RuntimeRep::Address],
        strict_fields: vec![false],
        layout: CheckedLayout {
            fields: vec![FieldLayout {
                rep: RuntimeRep::Address,
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![false],
        },
        tag: 1,
        family_size: 1,
    });
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        unreachable!()
    };
    top.binding.rhs = HeapRhs::Constructor {
        constructor: ConstructorId(0),
        fields: vec![Atom::Ref(ValueRef::Global(GlobalId(0)))],
    };
    let source = certify(
        wire,
        ImportOwner::Package {
            unit: "package".into(),
            module: "Package".into(),
            binder: identity(),
            interface_digest: DIGEST,
        },
    );
    let target = compile_target(BYTES);
    let demanded = DemandedImage::compile_with_package_literals(
        source,
        &ImageRegistry::new(),
        &tokens(&target),
    )
    .unwrap();
    assert!(demanded.image().heap_top_specs.is_empty());
    let descriptor = &demanded.image().interned_constructors[0].1;
    assert_eq!(descriptor.payload().fields()[0].rep(), RuntimeRep::Address);
    let mut machine = machine();
    let ids = install(&mut machine, demanded.image(), &target).unwrap();
    let handle = machine.retain_top(ids[0], ValueId(0)).unwrap();
    let pointer = machine.handle_current_pointer(handle).unwrap() & !7;
    // SAFETY: this retained static constructor has the admitted one-word
    // Address payload. Its descriptor marks the word untraced.
    let literal = unsafe { *((pointer + descriptor.payload_base() as usize) as *const u64) };
    assert_bytes(literal);
    assert!(!machine.import_slot_is_registered_root(ids[0], &identity()));
    machine.collect_major(machine.quiesce().unwrap()).unwrap();
    assert_bytes(literal);
    assert!(machine.release(handle));
    machine.collect_major(machine.quiesce().unwrap()).unwrap();
    assert_eq!(machine.residency().programs, 0);
}

fn machine() -> PreparedMachine<'static> {
    PreparedMachine::empty(PreparedMachineOptions {
        nursery_bytes: 4096,
    })
    .unwrap()
}

fn install(
    machine: &mut PreparedMachine<'_>,
    image: &Arc<CompiledProgram>,
    target: &Arc<CompiledProgram>,
) -> Result<Vec<crate::prepared_program::ProgramId>, ExecutionError> {
    machine.install_shared_batch(vec![
        BatchProgram {
            image: Arc::clone(image),
            imports: vec![BatchImport::Source {
                group: 1,
                binding: ValueId(1),
            }],
        },
        BatchProgram {
            image: Arc::clone(target),
            imports: vec![],
        },
    ])
}

fn call() -> PreparedCallOptions {
    PreparedCallOptions {
        observation_budget: 0,
        collect_before_observation: true,
    }
}

fn address(machine: &mut PreparedMachine<'_>, id: crate::prepared_program::ProgramId) -> u64 {
    let result = machine
        .run_entry_retained(id, ValueId(0), &[], call(), RealmId::ROOT)
        .unwrap();
    let [PreparedResult::Scalar(address)] = result.values.as_slice() else {
        panic!("literal reader returns one scalar address");
    };
    *address
}

fn assert_bytes(address: u64) {
    // SAFETY: the tested image/machine literal pool owns this exact admitted
    // allocation, including the implicit trailing NUL, throughout the read.
    let actual = unsafe { std::slice::from_raw_parts(address as *const u8, BYTES.len() + 1) };
    assert_eq!(&actual[..BYTES.len()], BYTES);
    assert_eq!(actual[BYTES.len()], 0);
}

#[test]
fn package_literal_dynamic_and_static_capture_are_untraced_and_owned() {
    let target = compile_target(BYTES);
    let supplied = tokens(&target);
    let registry = ImageRegistry::new();
    for captured in [false, true] {
        let source = group(captured, DIGEST, true);
        let original = source.clone();
        let demanded =
            DemandedImage::compile_with_package_literals(source, &registry, &supplied).unwrap();
        assert_eq!(
            demanded.group(),
            &original,
            "certified body and GlobalIds stay unchanged"
        );
        assert!(
            demanded.image().heap_top_specs.is_empty(),
            "immutable literal captures use static storage"
        );
        let mut machine = machine();
        let ids = install(&mut machine, demanded.image(), &target).unwrap();
        machine.pin(ids[0]).unwrap();
        assert_eq!(
            machine.import_slot_is_registered_root(ids[0], &identity()),
            false
        );
        let before = address(&mut machine, ids[0]);
        assert_bytes(before);
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert!(
            retired.programs.contains(&ids[1]),
            "byte producer is not a managed root"
        );
        assert_eq!(address(&mut machine, ids[0]), before);
        assert!(machine.unpin(ids[0]));
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 0);
        assert_bytes(before);
    }
}

#[test]
fn package_literal_contract_and_full_content_cache_key_are_exact() {
    let target = compile_target(BYTES);
    let supplied = tokens(&target);
    let registry = ImageRegistry::new();
    assert!(matches!(
        DemandedImage::compile(group(false, DIGEST, true), &registry),
        Err(DemandError::Compile(CompileError::Unsupported(_)))
    ));
    assert!(target.package_literals(|_, _| Some([0; 32])).is_empty());
    for (digest, evaluated) in [([8; 32], true), (DIGEST, false)] {
        assert!(matches!(
            DemandedImage::compile_with_package_literals(
                group(false, digest, evaluated),
                &registry,
                &supplied
            ),
            Err(DemandError::Compile(CompileError::PackageLiteralContract(
                _
            )))
        ));
    }
    let first = DemandedImage::compile_with_package_literals(
        group(false, DIGEST, true),
        &registry,
        &supplied,
    )
    .unwrap();
    let equal = compile_target(BYTES);
    let equal = DemandedImage::compile_with_package_literals(
        group(false, DIGEST, true),
        &registry,
        &tokens(&equal),
    )
    .unwrap();
    assert!(Arc::ptr_eq(first.image(), equal.image()));
    let different = compile_target(b"False");
    let different = DemandedImage::compile_with_package_literals(
        group(false, DIGEST, true),
        &registry,
        &tokens(&different),
    )
    .unwrap();
    assert!(
        !Arc::ptr_eq(first.image(), different.image()),
        "actual byte changes cannot reuse the image"
    );
}

#[test]
fn package_literal_batch_rejects_different_source_before_mutation() {
    let target = compile_target(BYTES);
    let registry = ImageRegistry::new();
    let demanded = DemandedImage::compile_with_package_literals(
        group(true, DIGEST, true),
        &registry,
        &tokens(&target),
    )
    .unwrap();
    let wrong = compile_target(b"False");
    let mut machine = machine();
    let before = machine.residency();
    assert!(matches!(
        install(&mut machine, demanded.image(), &wrong),
        Err(ExecutionError::BatchSourceContract(_))
    ));
    assert_eq!(machine.residency(), before);
    let ids = install(&mut machine, demanded.image(), &target).unwrap();
    machine.pin(ids[0]).unwrap();
    assert_bytes(address(&mut machine, ids[0]));
}

#[test]
fn package_literal_batch_rejects_managed_source_and_existing_handle() {
    let target = compile_target(BYTES);
    let registry = ImageRegistry::new();
    let demanded = DemandedImage::compile_with_package_literals(
        group(false, DIGEST, true),
        &registry,
        &tokens(&target),
    )
    .unwrap();
    let mut wire = testing::wire_program();
    wire.expressions
        .nodes
        .push(wire.expressions.nodes[0].clone());
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: identity(),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![],
                captures: vec![],
                body: 1,
            },
        },
    }));
    let managed = Arc::new(
        CompiledProgram::compile_prepared_definitions(&testing::prepare(wire).unwrap()).unwrap(),
    );
    let mut machine = machine();
    let before = machine.residency();
    assert!(install(&mut machine, demanded.image(), &managed).is_err());
    assert_eq!(machine.residency(), before);
    let ids = machine
        .install_shared_batch(vec![BatchProgram {
            image: managed,
            imports: vec![],
        }])
        .unwrap();
    let handle = machine.retain_top(ids[0], ValueId(1)).unwrap();
    let before = machine.residency();
    assert!(matches!(
        machine.install_shared_batch(vec![BatchProgram {
            image: Arc::clone(demanded.image()),
            imports: vec![BatchImport::Existing {
                handle,
                entry_signature: None
            }],
        }]),
        Err(ExecutionError::BatchSourceContract(_))
    ));
    assert_eq!(machine.residency(), before);
    assert!(machine.release(handle));
    machine.collect_major(machine.quiesce().unwrap()).unwrap();
    assert_eq!(machine.residency().programs, 0);
}

fn closure_caller() -> Arc<CompiledProgram> {
    let mut wire = testing::wire_program();
    wire.signatures[0] = Signature {
        arguments: vec![RuntimeRep::LiftedRef],
        results: ResultContract::Returns(vec![RuntimeRep::Address]),
    };
    wire.signatures.push(Signature {
        arguments: vec![],
        results: ResultContract::Returns(vec![RuntimeRep::Address]),
    });
    wire.expressions.nodes[0] = ExprFrame::Call {
        callee: Atom::Ref(ValueRef::Local(ValueId(1))),
        signature: SignatureId(1),
        arguments: vec![],
    };
    if let Group::NonRecursive(top) = &mut wire.bindings[0] {
        if let HeapRhs::Function { parameters, .. } = &mut top.binding.rhs {
            *parameters = vec![ValueId(1)];
        }
    }
    Arc::new(
        CompiledProgram::compile_prepared_definitions(&testing::prepare(wire).unwrap()).unwrap(),
    )
}

#[test]
fn package_literal_specialization_does_not_admit_address_pointer_equality() {
    use tidepool_repr::execution_schema::{OperationDecl, OperationId, OperationIdentity};
    let mut wire = group_wire(false, true);
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Int(64)]);
    wire.signatures.push(Signature {
        arguments: vec![RuntimeRep::Address, RuntimeRep::Address],
        results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
    });
    wire.operations.push(OperationDecl {
        identity: OperationIdentity::PrimOp("eqAddr#".into()),
        signature: SignatureId(1),
    });
    wire.expressions.nodes[0] = ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![Atom::Ref(ValueRef::Global(GlobalId(0))); 2],
    };
    let source = certify(
        wire,
        ImportOwner::Package {
            unit: "package".into(),
            module: "Package".into(),
            binder: identity(),
            interface_digest: DIGEST,
        },
    );
    let target = compile_target(BYTES);
    assert!(matches!(
        DemandedImage::compile_with_package_literals(
            source,
            &ImageRegistry::new(),
            &tokens(&target)
        ),
        Err(DemandError::Compile(CompileError::Unsupported(
            crate::prepared_program::Unsupported::Operation { .. }
        )))
    ));
}

#[test]
fn package_literal_parcel_keeps_image_bytes_after_sender_drop() {
    let target = compile_target(BYTES);
    let registry = ImageRegistry::new();
    let supplied = tokens(&target);
    let demanded = DemandedImage::compile_with_package_literals(
        group(true, DIGEST, true),
        &registry,
        &supplied,
    )
    .unwrap();
    let mut sender = machine();
    let ids = install(&mut sender, demanded.image(), &target).unwrap();
    let entry = sender.retain_top(ids[0], ValueId(0)).unwrap();
    let parcel = sender.export_parcel(entry).unwrap();
    assert_eq!(parcel.images().len(), 1);
    assert!(
        parcel.images()[0].imports.is_empty(),
        "internal literal slots issue no parcel handles"
    );
    drop(sender);
    drop(demanded);
    drop(supplied);
    drop(target);
    let mut receiver = machine();
    let ids = receiver
        .install_shared_batch(vec![BatchProgram {
            image: closure_caller(),
            imports: vec![],
        }])
        .unwrap();
    receiver.pin(ids[0]).unwrap();
    assert!(receiver
        .pending_parcel_import_identities(&parcel)
        .is_empty());
    let (arrived, imports) = receiver.import_parcel(parcel, RealmId::ROOT).unwrap();
    assert!(imports.is_empty());
    let result = receiver
        .run_entry_retained(
            ids[0],
            ValueId(0),
            &[PreparedInput::Managed(arrived)],
            call(),
            RealmId::ROOT,
        )
        .unwrap();
    let [PreparedResult::Scalar(address)] = result.values.as_slice() else {
        panic!("imported closure returns its byte address");
    };
    assert_bytes(*address);
    assert!(receiver.release(arrived));
    assert!(receiver.unpin(ids[0]));
    receiver.collect_major(receiver.quiesce().unwrap()).unwrap();
    assert_eq!(receiver.residency().programs, 0);
    assert_bytes(*address);
}
