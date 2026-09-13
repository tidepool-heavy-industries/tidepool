use super::CompiledProgram;
use cranelift_codegen::ir::{InstructionData, Opcode};
use tidepool_repr::execution_schema::{testing, *};

fn compile(wire: WireProgram) -> CompiledProgram {
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

#[test]
fn scalar_only_bytes_keep_the_exact_embedded_address_alive() {
    for payload in [Vec::new(), b"a\0b".to_vec()] {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = vec![RuntimeRep::Address];
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Bytes(payload.clone()))]);
        let program = compile(wire);

        assert!(program.byte_tops.is_empty());
        let storage = program.bytes.get(&payload).unwrap();
        assert_eq!(&storage[..payload.len()], payload);
        assert_eq!(storage.len(), payload.len() + 1);
        assert_eq!(storage[payload.len()], 0);

        let function_id = program.entries[&ValueId(0)].function;
        let function = &program.pipeline.emitted_ir.as_ref().unwrap()[&function_id];
        let address = storage.as_ptr() as i64;
        assert!(function.layout.blocks().any(|block| {
            function.layout.block_insts(block).any(|inst| {
                matches!(
                    &function.dfg.insts[inst],
                    InstructionData::UnaryImm { opcode: Opcode::Iconst, imm }
                        if i64::from(*imm) == address
                )
            })
        }));
    }
}

#[test]
fn bytes_top_and_scalar_literal_share_terminated_storage() {
    let payload = b"a\0b".to_vec();
    let mut wire = testing::wire_program();
    wire.signatures[0].results = vec![RuntimeRep::Address];
    wire.expressions.nodes[0] =
        ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Bytes(payload.clone()))]);
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("Bytes", "top"),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Bytes(payload.clone()),
        },
    }));
    let program = compile(wire);
    assert!(std::sync::Arc::ptr_eq(
        program.byte_tops.get(&ValueId(1)).unwrap(),
        program.bytes.get(&payload).unwrap(),
    ));
    assert_eq!(program.bytes[&payload].as_ref(), b"a\0b\0");
}

#[test]
fn heap_top_scalar_bytes_resolve_without_a_bytes_top() {
    let payload = b"heap\0field".to_vec();
    let mut wire = testing::wire_program();
    wire.signatures[0].results = vec![RuntimeRep::LiftedRef];
    wire.signatures.push(Signature {
        arguments: vec![],
        results: vec![RuntimeRep::LiftedRef],
    });
    wire.expressions.nodes[0] = ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: vec![],
    };
    wire.expressions.nodes.push(ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: vec![],
    });
    for (id, reps) in [vec![], vec![RuntimeRep::LiftedRef, RuntimeRep::Address]]
        .into_iter()
        .enumerate()
    {
        let layout = StorageLayout::for_reps(&wire.envelope.target, &reps).unwrap();
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("Bytes", &format!("C{id}")),
            family: testing::identity("Bytes", &format!("T{id}")),
            host_id: tidepool_repr::DataConId(960 + id as u64),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            strict_fields: vec![false; reps.len()],
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
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("Bytes", "caf"),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Thunk {
                signature: SignatureId(1),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body: 1,
            },
        },
    }));
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("Bytes", "holder"),
        binding: HeapBinding {
            id: ValueId(2),
            rhs: HeapRhs::Constructor {
                constructor: ConstructorId(1),
                fields: vec![
                    Atom::Ref(ValueRef::Local(ValueId(1))),
                    Atom::Scalar(ScalarLiteral::Bytes(payload.clone())),
                ],
            },
        },
    }));
    let program = compile(wire);
    assert!(program.byte_tops.is_empty());
    assert!(program
        .heap_top_specs
        .iter()
        .any(|spec| spec.id == ValueId(2)));

    let mut nursery = vec![0_u64; 128];
    let roots = super::invocation::RootWords::new(program.top_slots.len()).unwrap();
    let statics = program.statics.instantiate().unwrap();
    super::run::initialize_heap_tops(
        nursery.as_mut_ptr().cast(),
        nursery.len() * std::mem::size_of::<u64>(),
        &program.heap_top_specs,
        &program.top_slots,
        &roots,
        &statics,
        &program.byte_tops,
        &program.bytes,
    )
    .unwrap();

    let offset = program
        .heap_top_specs
        .iter()
        .take_while(|spec| spec.id != ValueId(2))
        .map(|spec| spec.descriptor.allocation_extent() as usize)
        .sum::<usize>();
    let holder = program
        .heap_top_specs
        .iter()
        .find(|spec| spec.id == ValueId(2))
        .unwrap();
    let address_slot = holder.descriptor.payload().logical_to_stored()[1].unwrap() as usize;
    let field = &holder.descriptor.payload().fields()[address_slot];
    let byte_offset = offset + holder.descriptor.payload_base() as usize + field.offset() as usize;
    let embedded = unsafe {
        nursery
            .as_ptr()
            .cast::<u8>()
            .add(byte_offset)
            .cast::<usize>()
            .read_unaligned()
    };
    assert_eq!(embedded, program.bytes[&payload].as_ptr() as usize);
    assert_eq!(program.bytes[&payload].as_ref(), b"heap\0field\0");
}
