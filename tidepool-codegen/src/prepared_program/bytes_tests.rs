use super::{CompileError, CompiledProgram, ExecutionError, RunOptions, Unsupported};
use crate::host_fns::RuntimeError;
use cranelift_codegen::ir::{InstructionData, Opcode};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_repr::execution_schema::{testing, *};

fn compile(wire: WireProgram) -> CompiledProgram {
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

fn resize_wire(new_len: i64, use_old_alias: bool) -> WireProgram {
    let mut wire = testing::wire_program();
    let reference = RuntimeRep::UnliftedRef;
    let int = RuntimeRep::Int(64);
    let word = RuntimeRep::Word(8);
    wire.signatures[0].results = ResultContract::Returns(vec![reference]);
    wire.signatures.extend([
        Signature {
            arguments: vec![int, RuntimeRep::Void],
            results: ResultContract::Returns(vec![reference]),
        },
        Signature {
            arguments: vec![reference, int, word, RuntimeRep::Void],
            results: ResultContract::Returns(vec![]),
        },
        Signature {
            arguments: vec![reference, int, RuntimeRep::Void],
            results: ResultContract::Returns(vec![reference]),
        },
        Signature {
            arguments: vec![reference, RuntimeRep::Void],
            results: ResultContract::Returns(vec![int]),
        },
    ]);
    wire.operations = [
        ("newByteArray#", 1),
        ("writeWord8Array#", 2),
        ("resizeMutableByteArray#", 3),
        ("getSizeofMutableByteArray#", 4),
    ]
    .into_iter()
    .map(|(name, signature)| OperationDecl {
        identity: OperationIdentity::PrimOp(name.into()),
        signature: SignatureId(signature),
    })
    .collect();
    let local = |id| Atom::Ref(ValueRef::Local(ValueId(id)));
    let integer = |value: i64| {
        Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    };
    let byte = |value: u8| {
        Atom::Scalar(ScalarLiteral::Word {
            bits: 8,
            bytes: vec![value],
        })
    };
    let operation = |id, arguments| ExprFrame::Operation {
        operation: OperationId(id),
        arguments,
    };
    let case = |scrutinee, binder, results: Vec<RuntimeRep>, binders: Vec<ValueId>, body| {
        ExprFrame::Case {
            scrutinee,
            binder: ValueId(binder),
            kind: CaseKind::MultiValue,
            scrutinee_results: ResultContract::Returns(results),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders,
                body,
            }],
        }
    };
    let mut nodes = vec![
        operation(0, vec![integer(2), Atom::Void]),
        operation(1, vec![local(100), integer(0), byte(0x7b), Atom::Void]),
        operation(1, vec![local(100), integer(1), byte(0x58), Atom::Void]),
        operation(2, vec![local(100), integer(new_len), Atom::Void]),
    ];
    let old_size = if use_old_alias {
        let index = nodes.len();
        nodes.push(operation(3, vec![local(100), Atom::Void]));
        Some(index)
    } else {
        None
    };
    let returned = nodes.len();
    nodes.push(ExprFrame::Return(vec![local(102)]));
    let after_resize = if let Some(old_size) = old_size {
        let index = nodes.len();
        nodes.push(case(old_size, 105, vec![int], vec![ValueId(104)], returned));
        index
    } else {
        returned
    };
    let resize_case = nodes.len();
    nodes.push(case(
        3,
        106,
        vec![reference],
        vec![ValueId(102)],
        after_resize,
    ));
    let write_one_case = nodes.len();
    nodes.push(case(2, 107, vec![], vec![], resize_case));
    let write_zero_case = nodes.len();
    nodes.push(case(1, 108, vec![], vec![], write_one_case));
    let new_case = nodes.len();
    nodes.push(case(
        0,
        109,
        vec![reference],
        vec![ValueId(100)],
        write_zero_case,
    ));
    wire.expressions.nodes = nodes;
    if let Group::NonRecursive(top) = &mut wire.bindings[0] {
        if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
            *body = new_case;
        }
    }
    wire
}

#[test]
fn resize_bytes_real_adapter_copies_prefix_zeroes_growth_and_survives_gc() {
    for (new_len, expected) in [(4, vec![0x7b, 0x58, 0, 0]), (1, vec![0x7b])] {
        let program = compile(resize_wire(new_len, false));
        let result = program
            .run_entry(
                ValueId(0),
                &[],
                &RunOptions {
                    nursery_bytes: 16,
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        // The first wrapper fills the 16-byte nursery, so the resize reserve
        // must collect before the explicit result-observation collection.
        assert!(result.collections >= 2);
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitByteArray(bytes))]
                if bytes == &expected
        ));
    }
}

#[test]
fn resize_bytes_old_alias_rejects_after_success() {
    let program = compile(resize_wire(4, true));
    let error = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 16,
                ..Default::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ExecutionError::Runtime(failure) if failure.cause == RuntimeError::BadPointer
    ));
}

#[test]
fn resize_bytes_invalid_lengths_leave_old_active_and_new_wrapper_empty() {
    use tidepool_heap::{
        execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind,
    };
    for (new_len, cause) in [
        (
            -1,
            RuntimeError::ArrayIndexOutOfBounds { index: -1, len: 2 },
        ),
        (i64::MAX, RuntimeError::HeapOverflow),
    ] {
        let descriptor = Arc::new(
            ObjectDescriptor::external(ExternalStorageKind::Bytes, &testing::target()).unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let machine = crate::machine_state::MachineState::new();
        machine
            .install_prepared_buffer(vec![0_u64; extent * 2 / 8], vec![descriptor.clone()])
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let mut vmctx = unsafe { crate::context::VMContext::new(start, start.add(size)) };
        vmctx.alloc_ptr = unsafe { start.add(extent * 2) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        let replacement_wrapper = unsafe { start.add(extent) };
        let reference = (start as usize | usize::from(descriptor.tag())) as *mut u8;
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 2)
            .unwrap();
        machine
            .store_external_bytes(payload, 0, &[0x7b, 0x58])
            .unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor.initialize_header(replacement_wrapper);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(payload);
        }
        let status = unsafe {
            super::byte_arrays::prepared_resize_bytes(
                &mut vmctx,
                reference,
                Arc::as_ptr(&descriptor),
                replacement_wrapper,
                new_len,
            )
        };
        assert_eq!(
            status,
            crate::prepared_control::CallStatus::LanguageFailure as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(cause));
        assert_eq!(
            machine.disposition(),
            crate::machine_state::MachineDisposition::Reusable
        );
        assert!(unsafe {
            descriptor
                .external_payload_slot(replacement_wrapper, extent)
                .unwrap()
                .read()
                .is_null()
        });
        assert_eq!(
            machine
                .external_active_view(payload, ExternalStorageKind::Bytes)
                .unwrap()
                .logical_len,
            2
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(payload.add(8), 2) },
            &[0x7b, 0x58]
        );
    }
}

fn byte_copy_compare_wire(
    source_offset: i64,
    destination_offset: i64,
    count: i64,
    perform_copy: bool,
    reverse_compare: bool,
) -> WireProgram {
    let mut wire = testing::wire_program();
    use RuntimeRep::{Int, UnliftedRef, Void, Word};
    wire.signatures[0].results = ResultContract::Returns(vec![UnliftedRef, Int(64)]);
    wire.signatures.extend([
        Signature {
            arguments: vec![Int(64), Void],
            results: ResultContract::Returns(vec![UnliftedRef]),
        },
        Signature {
            arguments: vec![UnliftedRef, Int(64), Word(8), Void],
            results: ResultContract::Returns(vec![]),
        },
        Signature {
            arguments: vec![UnliftedRef, Int(64), UnliftedRef, Int(64), Int(64), Void],
            results: ResultContract::Returns(vec![]),
        },
        Signature {
            arguments: vec![UnliftedRef, Int(64), UnliftedRef, Int(64), Int(64)],
            results: ResultContract::Returns(vec![Int(64)]),
        },
        Signature {
            arguments: vec![UnliftedRef, Void],
            results: ResultContract::Returns(vec![UnliftedRef]),
        },
    ]);
    wire.operations = [
        "newByteArray#",
        "writeWord8Array#",
        "copyByteArray#",
        "compareByteArrays#",
        "unsafeFreezeByteArray#",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, name)| OperationDecl {
        identity: OperationIdentity::PrimOp(name.into()),
        signature: SignatureId(index as u32 + 1),
    })
    .collect();
    let int = |value: i64| {
        Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    };
    let byte = |value: u8| {
        Atom::Scalar(ScalarLiteral::Word {
            bits: 8,
            bytes: vec![value],
        })
    };
    let local = |id| Atom::Ref(ValueRef::Local(ValueId(id)));
    let operation = |id, arguments| ExprFrame::Operation {
        operation: OperationId(id),
        arguments,
    };
    let case = |scrutinee, binder, kind, results, binders, body| ExprFrame::Case {
        scrutinee,
        binder: ValueId(binder),
        kind,
        scrutinee_results: ResultContract::Returns(results),
        alternatives: vec![Alternative {
            pattern: AlternativePattern::Default,
            binders,
            body,
        }],
    };
    let comparison = if reverse_compare {
        vec![
            local(101),
            int(destination_offset),
            local(100),
            int(source_offset),
            int(count),
        ]
    } else {
        vec![
            local(100),
            int(source_offset),
            local(101),
            int(destination_offset),
            int(count),
        ]
    };
    let copy_arguments = if perform_copy {
        vec![
            local(100),
            int(source_offset),
            local(101),
            int(destination_offset),
            int(count),
            Atom::Void,
        ]
    } else {
        vec![local(100), int(4), local(101), int(4), int(0), Atom::Void]
    };
    wire.expressions.nodes = vec![
        operation(0, vec![int(4), Atom::Void]),
        operation(1, vec![local(100), int(0), byte(b'a'), Atom::Void]),
        operation(1, vec![local(100), int(1), byte(0xff), Atom::Void]),
        operation(1, vec![local(100), int(2), byte(b'c'), Atom::Void]),
        operation(1, vec![local(100), int(3), byte(b'd'), Atom::Void]),
        operation(0, vec![int(4), Atom::Void]),
        operation(2, copy_arguments),
        operation(3, comparison),
        operation(4, vec![local(101), Atom::Void]),
        ExprFrame::Return(vec![local(103), local(102)]),
        case(
            8,
            208,
            CaseKind::MultiValue,
            vec![UnliftedRef],
            vec![ValueId(103)],
            9,
        ),
        case(7, 102, CaseKind::Polymorphic, vec![Int(64)], vec![], 10),
        case(6, 206, CaseKind::MultiValue, vec![], vec![], 11),
        case(
            5,
            205,
            CaseKind::MultiValue,
            vec![UnliftedRef],
            vec![ValueId(101)],
            12,
        ),
        case(4, 204, CaseKind::MultiValue, vec![], vec![], 13),
        case(3, 203, CaseKind::MultiValue, vec![], vec![], 14),
        case(2, 202, CaseKind::MultiValue, vec![], vec![], 15),
        case(1, 201, CaseKind::MultiValue, vec![], vec![], 16),
        case(
            0,
            200,
            CaseKind::MultiValue,
            vec![UnliftedRef],
            vec![ValueId(100)],
            17,
        ),
    ];
    if let Group::NonRecursive(top) = &mut wire.bindings[0] {
        if let HeapRhs::Function { body, .. } = &mut top.binding.rhs {
            *body = 18;
        }
    }
    wire
}

#[test]
fn byte_copy_compare_real_adapter_handles_interior_and_empty_spans_across_gc() {
    for (source_offset, destination_offset, count, perform_copy, reverse, expected, ordering) in [
        (1, 1, 2, true, false, &[0, 0xff, b'c', 0][..], 0),
        (4, 4, 0, true, false, &[0, 0, 0, 0][..], 0),
        (1, 1, 1, false, false, &[0, 0, 0, 0][..], 1),
        (1, 1, 1, false, true, &[0, 0, 0, 0][..], -1),
    ] {
        let program = compile(byte_copy_compare_wire(
            source_offset,
            destination_offset,
            count,
            perform_copy,
            reverse,
        ));
        let result = program
            .run_entry(
                ValueId(0),
                &[],
                &RunOptions {
                    nursery_bytes: 16,
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(result.collections >= 2);
        assert!(matches!(
            result.values.as_slice(),
            [
                tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitByteArray(bytes)),
                tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(actual_ordering)),
            ] if bytes == expected && *actual_ordering == ordering
        ));
    }
}

#[test]
fn byte_copy_compare_hosts_reject_bad_ranges_alias_copy_and_revocation() {
    use crate::prepared_control::CallStatus;
    use tidepool_heap::{
        execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind,
    };

    for (source_offset, destination_offset, count, alias, revoked, expected) in [
        (
            3,
            0,
            2,
            false,
            false,
            RuntimeError::ArrayIndexOutOfBounds { index: 4, len: 4 },
        ),
        (
            0,
            3,
            2,
            false,
            false,
            RuntimeError::ArrayIndexOutOfBounds { index: 4, len: 4 },
        ),
        (
            -1,
            0,
            1,
            false,
            false,
            RuntimeError::ArrayIndexOutOfBounds { index: -1, len: 4 },
        ),
        (0, 0, 1, true, false, RuntimeError::AliasedByteCopy),
        (0, 0, 1, false, true, RuntimeError::BadPointer),
    ] {
        let descriptor = Arc::new(
            ObjectDescriptor::external(ExternalStorageKind::Bytes, &testing::target()).unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let machine = crate::machine_state::MachineState::new();
        machine
            .install_prepared_buffer(vec![0_u64; extent * 2 / 8], vec![descriptor.clone()])
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let second = unsafe { start.add(extent) };
        let mut vmctx = unsafe { crate::context::VMContext::new(start, start.add(size)) };
        vmctx.alloc_ptr = unsafe { start.add(extent * 2) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        let source_ref = (start as usize | usize::from(descriptor.tag())) as *mut u8;
        let destination_ref = (second as usize | usize::from(descriptor.tag())) as *mut u8;
        let source = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        let destination = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        machine.store_external_bytes(source, 0, b"abcd").unwrap();
        machine
            .store_external_bytes(destination, 0, b"zzzz")
            .unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor.initialize_header(second);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(source);
            descriptor
                .external_payload_slot(second, extent)
                .unwrap()
                .write(destination);
        }
        if revoked {
            machine
                .revoke_external_payload(source, ExternalStorageKind::Bytes)
                .unwrap();
        }
        let before = machine.external_storage_stats();
        let status = unsafe {
            super::byte_arrays::prepared_copy_bytes(
                &mut vmctx,
                Arc::as_ptr(&descriptor),
                source_ref,
                source_offset,
                if alias { source_ref } else { destination_ref },
                destination_offset,
                count,
            )
        };
        assert_eq!(
            status,
            if revoked {
                CallStatus::IntegrityFailure as i32
            } else {
                CallStatus::LanguageFailure as i32
            }
        );
        assert_eq!(machine.take_runtime_error(), Some(expected.clone()));
        assert_eq!(machine.external_storage_stats(), before);
        assert_eq!(machine.copy_external_bytes(destination).unwrap(), b"zzzz");
    }
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
fn literal_addresses_observe_through_one_shot_and_other_installed_programs() {
    use super::{ImportBindings, PreparedCallOptions, PreparedMachine, PreparedMachineOptions};
    use crate::suspension::RealmId;
    let payload = b"ab\0tail".to_vec();
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
    wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: testing::identity("AddressOwner", "bytes"),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Bytes(payload.clone()),
        },
    }));
    let owner = compile(wire);
    let address = owner.bytes.get(&payload).unwrap().as_ptr() as usize;
    let result = owner
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(
        matches!(&result.values[0], tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitString(bytes)) if bytes == &payload)
    );

    let (mut machine, _) = PreparedMachine::new(
        owner,
        PreparedMachineOptions {
            nursery_bytes: RunOptions::default().nursery_bytes,
        },
    )
    .unwrap();
    let mut wire = testing::wire_program();
    wire.signatures[0] = Signature {
        arguments: vec![RuntimeRep::Address],
        results: ResultContract::Returns(vec![RuntimeRep::Address]),
    };
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
    wire.bindings = vec![Group::NonRecursive(TopBinding {
        identity: testing::identity("AddressReader", "entry"),
        binding: HeapBinding {
            id: ValueId(0),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(1)],
                captures: vec![],
                body: 0,
            },
        },
    })];
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    let reader = machine.compile_for_install(&linked).unwrap();
    let reader = machine
        .install_program(reader, ImportBindings::new())
        .unwrap();
    let result = machine
        .run_entry(
            reader,
            ValueId(0),
            &[(address + 2) as u64],
            PreparedCallOptions {
                observation_budget: 6,
                collect_before_observation: true,
            },
            RealmId::ROOT,
        )
        .unwrap();
    assert!(
        matches!(&result.values[0], tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitString(bytes)) if bytes == &payload[2..])
    );

    // String primitives resolve a literal address through the same machine
    // authority as observation, whichever installed program owns the bytes.
    let linked = link_program(
        testing::prepare(c_string_len_wire(
            c_string_len_identity(),
            c_string_len_signature(),
        ))
        .unwrap(),
        &MachineImports::default(),
    )
    .unwrap();
    let strlen = machine.compile_for_install(&linked).unwrap();
    let strlen = machine
        .install_program(strlen, ImportBindings::new())
        .unwrap();
    let length = machine
        .run_entry(
            strlen,
            ValueId(0),
            &[address as u64],
            PreparedCallOptions {
                observation_budget: 1,
                collect_before_observation: false,
            },
            RealmId::ROOT,
        )
        .unwrap();
    assert!(matches!(
        length.values.as_slice(),
        [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(
            2
        ))]
    ));
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
    let roots = super::roots::RootWords::new(program.top_slots.len()).unwrap();
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
        &program.import_slots,
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
        [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(value))] => Ok(*value),
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
            Err(CompileError::Unsupported(Unsupported::Operation { .. }))
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
            Err(CompileError::Unsupported(Unsupported::Operation { .. }))
        ));
    }
}

fn copy_addr_wire(destination_len: i64, offset: i64, count: i64) -> WireProgram {
    let mut wire = testing::wire_program();
    wire.signatures = vec![
        Signature {
            arguments: vec![RuntimeRep::Address],
            results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
        },
        Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
        },
        Signature {
            arguments: vec![
                RuntimeRep::Address,
                RuntimeRep::UnliftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
                RuntimeRep::Void,
            ],
            results: ResultContract::Returns(vec![]),
        },
        Signature {
            arguments: vec![RuntimeRep::UnliftedRef, RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
        },
    ];
    wire.operations = [
        "newByteArray#",
        "copyAddrToByteArray#",
        "unsafeFreezeByteArray#",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, name)| OperationDecl {
        identity: OperationIdentity::PrimOp(name.into()),
        signature: SignatureId(index as u32 + 1),
    })
    .collect();
    let int = |value: i64| {
        Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    };
    let local = |id| Atom::Ref(ValueRef::Local(ValueId(id)));
    let operation = |id, arguments| ExprFrame::Operation {
        operation: OperationId(id),
        arguments,
    };
    let case = |scrutinee, binder, results, binders, body| ExprFrame::Case {
        scrutinee,
        binder: ValueId(binder),
        kind: CaseKind::MultiValue,
        scrutinee_results: ResultContract::Returns(results),
        alternatives: vec![Alternative {
            pattern: AlternativePattern::Default,
            binders,
            body,
        }],
    };
    wire.expressions.nodes = vec![
        operation(0, vec![int(destination_len), Atom::Void]),
        operation(
            1,
            vec![local(1), local(100), int(offset), int(count), Atom::Void],
        ),
        operation(2, vec![local(100), Atom::Void]),
        ExprFrame::Return(vec![local(101)]),
        case(2, 102, vec![RuntimeRep::UnliftedRef], vec![ValueId(101)], 3),
        case(1, 103, vec![], vec![], 4),
        case(0, 104, vec![RuntimeRep::UnliftedRef], vec![ValueId(100)], 5),
    ];
    wire.bindings = vec![
        Group::NonRecursive(TopBinding {
            identity: testing::identity("CopyAddr", "entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![ValueId(1)],
                    captures: vec![],
                    body: 6,
                },
            },
        }),
        Group::NonRecursive(TopBinding {
            identity: testing::identity("CopyAddr", "storage"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Bytes(b"abcdef".to_vec()),
            },
        }),
    ];
    wire
}

#[test]
fn copy_addr_real_adapter_handles_full_interior_and_empty_spans_after_gc() {
    for (length, source_shift, offset, count, expected) in [
        (6, 0, 0, 6, b"abcdef".as_slice()),
        (5, 1, 1, 3, b"\0bcd\0".as_slice()),
        (3, 7, 3, 0, b"\0\0\0".as_slice()),
    ] {
        let program = compile(copy_addr_wire(length, offset, count));
        let storage = program.bytes.get(b"abcdef").unwrap();
        let address = storage.as_ptr() as usize + source_shift;
        let result = program
            .run_entry(
                ValueId(0),
                &[address as u64],
                &RunOptions {
                    nursery_bytes: 128,
                    collect_before_observation: true,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(result.collections >= 1);
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitByteArray(bytes))]
                if bytes == expected
        ));
    }
}

#[test]
fn copy_addr_real_adapter_rejects_unowned_source_and_destination_overrun() {
    for (address, offset, count, expected) in [
        (None, 0, 1, RuntimeError::BadPointer),
        (Some(6), 0, 2, RuntimeError::BadPointer),
        (
            Some(0),
            3,
            2,
            RuntimeError::ArrayIndexOutOfBounds { index: 4, len: 4 },
        ),
    ] {
        let program = compile(copy_addr_wire(4, offset, count));
        let storage = program.bytes.get(b"abcdef").unwrap();
        let address = address.map_or(0, |shift| storage.as_ptr() as usize + shift);
        let error = program
            .run_entry(
                ValueId(0),
                &[address as u64],
                &RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            ExecutionError::Runtime(crate::machine_state::MachineFailure { cause, .. })
                if cause == expected
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
        [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitWord(value))] => Ok(*value),
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
        Err(CompileError::Unsupported(Unsupported::Operation { .. }))
    ));
}
