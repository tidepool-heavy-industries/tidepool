use super::{CompileError, CompiledProgram, ExecutionError, RunOptions, Unsupported};
use crate::host_fns::RuntimeError;
use cranelift_codegen::ir::{InstructionData, Opcode};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_repr::execution_schema::{testing, *};

fn compile(wire: WireProgram) -> CompiledProgram {
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

#[test]
fn scalar_only_bytes_keep_the_exact_embedded_address_alive() {
    for payload in [Vec::new(), b"a\0b".to_vec()] {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
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
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
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
    assert_eq!(program.bytes.get(&payload).unwrap().as_ref(), b"a\0b\0");
}

#[test]
fn heap_top_scalar_bytes_resolve_without_a_bytes_top() {
    let payload = b"heap\0field".to_vec();
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.signatures.push(Signature {
        arguments: vec![],
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
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
    assert_eq!(
        embedded,
        program.bytes.get(&payload).unwrap().as_ptr() as usize
    );
    assert_eq!(
        program.bytes.get(&payload).unwrap().as_ref(),
        b"heap\0field\0"
    );
}

fn index_char_wire(result_rep: RuntimeRep) -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![RuntimeRep::Address, RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![result_rep]),
        },
        Signature {
            arguments: vec![RuntimeRep::Address, RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![result_rep]),
        },
    ];
    wire.operations = vec![OperationDecl {
        identity: OperationIdentity::PrimOp("indexCharOffAddr#".into()),
        signature: SignatureId(1),
    }];
    wire.expressions.nodes[0] = ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![
            Atom::Ref(ValueRef::Local(ValueId(1))),
            Atom::Ref(ValueRef::Local(ValueId(2))),
        ],
    };
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("IndexChar", "entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![ValueId(1), ValueId(2)],
                    captures: vec![],
                    body: 0,
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("IndexChar", "storage"),
            binding: HeapBinding {
                id: ValueId(3),
                rhs: HeapRhs::Bytes(b"\x80A".to_vec()),
            },
        }),
    ];
    wire
}

fn c_string_len_wire(operation: OperationIdentity, operation_signature: Signature) -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![RuntimeRep::Address],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        },
        operation_signature,
    ];
    wire.operations = vec![OperationDecl {
        identity: operation,
        signature: SignatureId(1),
    }];
    wire.expressions.nodes[0] = ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1))), Atom::Void],
    };
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("CStringLen", "entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![ValueId(1)],
                    captures: vec![],
                    body: 0,
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("CStringLen", "storage"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Bytes(b"ab\0tail".to_vec()),
            },
        }),
    ];
    wire
}

fn c_string_len_identity() -> OperationIdentity {
    OperationIdentity::Intrinsic {
        symbol: "strlen".into(),
        convention: ForeignConvention::CCall,
    }
}

fn c_string_len_signature() -> Signature {
    Signature {
        arguments: vec![RuntimeRep::Address, RuntimeRep::Void],
        results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
    }
}

fn c_string_len(program: &CompiledProgram, address: usize) -> Result<i64, ExecutionError> {
    let result = program.run_entry(
        ValueId(0),
        &[address as u64],
        &RunOptions::default(),
        Arc::new(AtomicBool::new(false)),
    )?;
    match result.values.as_slice() {
        [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(value))] => Ok(*value),
        other => panic!("unexpected strlen result: {other:?}"),
    }
}

#[test]
fn c_string_len_real_adapter_stops_at_owned_nul_and_rejects_unknown_addresses() {
    let program = compile(c_string_len_wire(
        c_string_len_identity(),
        c_string_len_signature(),
    ));
    let storage = program.bytes.get(b"ab\0tail").unwrap();
    let base = storage.as_ptr() as usize;
    assert_eq!(c_string_len(&program, base).unwrap(), 2);
    assert_eq!(c_string_len(&program, base + 1).unwrap(), 1);
    assert_eq!(c_string_len(&program, base + 2).unwrap(), 0);
    assert_eq!(c_string_len(&program, base + 3).unwrap(), 4);
    assert_eq!(c_string_len(&program, base + storage.len() - 1).unwrap(), 0);
    for address in [0, usize::MAX, base + storage.len()] {
        assert!(matches!(
            c_string_len(&program, address),
            Err(ExecutionError::Runtime(
                crate::machine_state::MachineFailure {
                    cause: RuntimeError::BadPointer,
                    ..
                }
            ))
        ));
    }
}

#[test]
fn c_string_len_requires_exact_intrinsic_identity_and_signature() {
    let valid = c_string_len_signature();
    let invalid = [
        Signature {
            arguments: vec![RuntimeRep::Address],
            ..valid.clone()
        },
        Signature {
            arguments: vec![RuntimeRep::Address, RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::Word(64)]),
        },
    ];
    for signature in invalid {
        let mut wire = c_string_len_wire(c_string_len_identity(), signature.clone());
        wire.signatures[0].results = signature.results.clone();
        if signature.arguments.len() == 1 {
            wire.expressions.nodes[0] = ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
            };
        }
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        assert!(matches!(
            CompiledProgram::compile(&linked),
            Err(CompileError::Unsupported(Unsupported::Expression { .. }))
        ));
    }
    for identity in [
        OperationIdentity::PrimOp("strlen".into()),
        OperationIdentity::Intrinsic {
            symbol: "other_strlen".into(),
            convention: ForeignConvention::CCall,
        },
    ] {
        let linked = link_program(
            testing::prepare(c_string_len_wire(identity, valid.clone())).unwrap(),
            &MachineImports::default(),
        )
        .unwrap();
        assert!(matches!(
            CompiledProgram::compile(&linked),
            Err(CompileError::Unsupported(Unsupported::Expression { .. }))
        ));
    }
}

fn index_char(
    program: &CompiledProgram,
    address: usize,
    offset: i64,
) -> Result<u64, ExecutionError> {
    let result = program.run_entry(
        ValueId(0),
        &[address as u64, offset as u64],
        &RunOptions::default(),
        Arc::new(AtomicBool::new(false)),
    )?;
    match result.values.as_slice() {
        [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(value))] => Ok(*value),
        other => panic!("unexpected indexCharOffAddr# result: {other:?}"),
    }
}

fn assert_bad_pointer(result: Result<u64, ExecutionError>) {
    assert!(matches!(
        result,
        Err(ExecutionError::Runtime(
            crate::machine_state::MachineFailure {
                cause: RuntimeError::BadPointer,
                ..
            }
        ))
    ));
}

#[test]
fn index_char_real_adapter_reads_owned_bytes_and_terminal_nul() {
    let program = compile(index_char_wire(RuntimeRep::Word(64)));
    let storage = program.bytes.get(b"\x80A").unwrap();
    let base = storage.as_ptr() as usize;
    assert_eq!(index_char(&program, base, 0).unwrap(), 0x80);
    assert_eq!(index_char(&program, base, 1).unwrap(), b'A' as u64);
    assert_eq!(index_char(&program, base, 2).unwrap(), 0);
    assert_eq!(index_char(&program, base + 1, -1).unwrap(), 0x80);
    assert_eq!(index_char(&program, base + storage.len(), -1).unwrap(), 0);
}

#[test]
fn index_char_real_adapter_rejects_out_of_range_and_unowned_addresses() {
    let program = compile(index_char_wire(RuntimeRep::Word(64)));
    let storage = program.bytes.get(b"\x80A").unwrap();
    let base = storage.as_ptr() as usize;
    assert_bad_pointer(index_char(&program, base, -1));
    assert_bad_pointer(index_char(&program, base, storage.len() as i64));
    assert_bad_pointer(index_char(&program, base + storage.len(), 0));
    assert_bad_pointer(index_char(&program, 0, 0));
    assert_bad_pointer(index_char(&program, usize::MAX, 1));
    assert_eq!(index_char(&program, base, 1).unwrap(), b'A' as u64);
}

#[test]
fn index_char_rejects_wrong_char_rep_before_native_emission() {
    let linked = link_program(
        testing::prepare(index_char_wire(RuntimeRep::Word(32))).unwrap(),
        &MachineImports::default(),
    )
    .unwrap();
    assert!(matches!(
        CompiledProgram::compile(&linked),
        Err(CompileError::Unsupported(Unsupported::Expression { .. }))
    ));
}
