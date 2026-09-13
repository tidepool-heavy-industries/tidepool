use std::collections::BTreeMap;
use std::sync::{Arc, atomic::AtomicBool};

use super::{
    CompileError, CompiledProgram, ExecutionError, ObservationFailure, RunOptions, Unsupported,
};
use cranelift_codegen::ir::{self, InstructionData, Opcode, ValueDef};
use tidepool_bridge::Value;
use tidepool_repr::execution_schema::{
    Architecture, DecodeLimits, EXECUTION_ABI_VERSION, Endianness, MachineImports,
    ProgramRequirements, RuntimeRep, SCHEMA_VERSION, TargetDescriptor, link_program, parse_program,
};
use tidepool_repr::{DataConId, Literal};

fn head(major: u8, length: usize) -> Vec<u8> {
    let mut result = Vec::new();
    if length <= 23 {
        result.push((major << 5) | length as u8);
    } else if length <= u8::MAX as usize {
        result.extend([(major << 5) | 24, length as u8]);
    } else if length <= u16::MAX as usize {
        result.push((major << 5) | 25);
        result.extend((length as u16).to_be_bytes());
    } else if length <= u32::MAX as usize {
        result.push((major << 5) | 26);
        result.extend((length as u32).to_be_bytes());
    } else {
        result.push((major << 5) | 27);
        result.extend((length as u64).to_be_bytes());
    }
    result
}

fn array(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let values: Vec<_> = values.into_iter().collect();
    let mut result = head(4, values.len());
    for value in values {
        result.extend(value);
    }
    result
}

fn uint(value: u64) -> Vec<u8> {
    if value <= 23 {
        vec![value as u8]
    } else if value <= u8::MAX as u64 {
        vec![0x18, value as u8]
    } else if value <= u16::MAX as u64 {
        let mut result = vec![0x19];
        result.extend((value as u16).to_be_bytes());
        result
    } else if value <= u32::MAX as u64 {
        let mut result = vec![0x1a];
        result.extend((value as u32).to_be_bytes());
        result
    } else {
        let mut result = vec![0x1b];
        result.extend(value.to_be_bytes());
        result
    }
}

fn boolean(value: bool) -> Vec<u8> {
    vec![if value { 0xf5 } else { 0xf4 }]
}

fn text(value: &str) -> Vec<u8> {
    let mut result = head(3, value.len());
    result.extend(value.as_bytes());
    result
}

fn bytes(value: &[u8]) -> Vec<u8> {
    let mut result = head(2, value.len());
    result.extend(value);
    result
}

fn rep(rep: RuntimeRep) -> Vec<u8> {
    match rep {
        RuntimeRep::Void => array([uint(0)]),
        RuntimeRep::LiftedRef => array([uint(1)]),
        RuntimeRep::Int(bits) => array([uint(4), uint(u64::from(bits))]),
        RuntimeRep::Word(bits) => array([uint(5), uint(u64::from(bits))]),
        RuntimeRep::Float(bits) => array([uint(6), uint(u64::from(bits))]),
        other => panic!("fixture does not encode {other:?}"),
    }
}

fn signature(results: &[RuntimeRep]) -> Vec<u8> {
    signature_with(&[], results)
}

fn signature_with(arguments: &[RuntimeRep], results: &[RuntimeRep]) -> Vec<u8> {
    array([
        array(arguments.iter().copied().map(rep)),
        array(results.iter().copied().map(rep)),
    ])
}

fn symbol(name: &str) -> Vec<u8> {
    array([
        text("fixture"),
        text("AbiContract"),
        text("value"),
        text(name),
    ])
}

fn scalar_int(value: u64) -> Vec<u8> {
    array([uint(0), uint(64), bytes(&value.to_be_bytes())])
}

fn scalar_word(value: u64) -> Vec<u8> {
    array([uint(1), uint(64), bytes(&value.to_be_bytes())])
}

fn scalar_float64(bits: u64) -> Vec<u8> {
    array([uint(2), uint(64), bytes(&bits.to_be_bytes())])
}

fn atom_ref(id: u8) -> Vec<u8> {
    array([uint(0), value_ref_local(id)])
}

fn value_ref_local(id: u8) -> Vec<u8> {
    array([uint(0), uint(u64::from(id))])
}

fn atom_scalar(scalar: Vec<u8>) -> Vec<u8> {
    array([uint(1), scalar])
}

fn return_frame(atoms: Vec<Vec<u8>>) -> Vec<u8> {
    array([uint(0), array(atoms)])
}

fn jump_frame(join: u8, arguments: Vec<Vec<u8>>) -> Vec<u8> {
    array([uint(8), uint(u64::from(join)), array(arguments)])
}

fn heap_binding(id: u8, rhs: Vec<u8>) -> Vec<u8> {
    array([uint(u64::from(id)), rhs])
}

fn group_nonrecursive(binding: Vec<u8>) -> Vec<u8> {
    array([uint(0), binding])
}

fn group_recursive(bindings: Vec<Vec<u8>>) -> Vec<u8> {
    array([uint(1), array(bindings)])
}

fn let_frame(binding: Vec<u8>, body: u8) -> Vec<u8> {
    array([uint(6), group_nonrecursive(binding), uint(u64::from(body))])
}

fn let_recursive_frame(bindings: Vec<Vec<u8>>, body: u8) -> Vec<u8> {
    array([uint(6), group_recursive(bindings), uint(u64::from(body))])
}

fn let_joins_frame(binding: Vec<u8>, body: u8) -> Vec<u8> {
    array([uint(7), group_nonrecursive(binding), uint(u64::from(body))])
}

fn call_frame(callee: u8, signature: u8) -> Vec<u8> {
    array([
        uint(2),
        atom_ref(callee),
        uint(u64::from(signature)),
        array([]),
    ])
}

fn enter_frame(callee: u8, signature: u8) -> Vec<u8> {
    array([uint(1), atom_ref(callee), uint(u64::from(signature))])
}

fn case_kind(kind: u8) -> Vec<u8> {
    match kind {
        0 => array([uint(0), symbol("BoxFamily")]),
        1 => array([uint(1), rep(RuntimeRep::Int(64))]),
        2 => array([uint(2)]),
        3 => array([uint(3)]),
        _ => panic!("unknown case kind {kind}"),
    }
}

fn default_pattern() -> Vec<u8> {
    array([uint(0)])
}

fn constructor_pattern(constructor: u8) -> Vec<u8> {
    array([uint(1), uint(u64::from(constructor))])
}

fn literal_pattern(scalar: Vec<u8>) -> Vec<u8> {
    array([uint(2), scalar])
}

fn alternative(pattern: Vec<u8>, binders: Vec<u8>, body: u8) -> Vec<u8> {
    array([
        pattern,
        array(binders.into_iter().map(|id| uint(u64::from(id)))),
        uint(u64::from(body)),
    ])
}

fn case_frame(
    scrutinee: u8,
    binder: u8,
    reps: &[RuntimeRep],
    kind: u8,
    alternatives: Vec<Vec<u8>>,
) -> Vec<u8> {
    array([
        uint(5),
        uint(u64::from(scrutinee)),
        uint(u64::from(binder)),
        array(reps.iter().copied().map(rep)),
        case_kind(kind),
        array(alternatives),
    ])
}

fn construct_frame(constructor: u8) -> Vec<u8> {
    array([uint(4), uint(u64::from(constructor)), array([])])
}

fn constructor_rhs() -> Vec<u8> {
    array([uint(2), uint(0), array([])])
}

fn function_rhs(signature: u8, captures: Vec<Vec<u8>>, body: u8) -> Vec<u8> {
    array([
        uint(0),
        uint(u64::from(signature)),
        array([]),
        array(captures),
        uint(u64::from(body)),
    ])
}

fn join_binding(id: u8, signature: u8, parameter: u8, body: u8) -> Vec<u8> {
    array([
        uint(u64::from(id)),
        uint(u64::from(signature)),
        array([uint(u64::from(parameter))]),
        uint(u64::from(body)),
    ])
}

fn top_binding(rhs: Vec<u8>) -> Vec<u8> {
    top_binding_named(0, "entry", rhs)
}

fn top_binding_named(id: u8, name: &str, rhs: Vec<u8>) -> Vec<u8> {
    array([symbol(name), heap_binding(id, rhs)])
}

fn constructor_decl() -> Vec<u8> {
    array([
        symbol("Box"),
        symbol("BoxFamily"),
        array([]),
        array([]),
        array([array([]), uint(1), uint(0), array([])]),
        rep(RuntimeRep::LiftedRef),
        uint(1),
        uint(1),
        uint(100),
    ])
}

fn lifted_constructor_decl() -> Vec<u8> {
    array([
        symbol("Outer"),
        symbol("OuterFamily"),
        array([rep(RuntimeRep::LiftedRef)]),
        array([boolean(false)]),
        array([
            array([array([rep(RuntimeRep::LiftedRef), uint(0)])]),
            uint(8),
            uint(8),
            array([boolean(true)]),
        ]),
        rep(RuntimeRep::LiftedRef),
        uint(1),
        uint(1),
        uint(101),
    ])
}

fn constructor_rhs_fields(constructor: u8, fields: Vec<Vec<u8>>) -> Vec<u8> {
    array([uint(2), uint(u64::from(constructor)), array(fields)])
}

fn wire_program_with_bindings(
    signatures: Vec<Vec<u8>>,
    constructors: Vec<Vec<u8>>,
    expressions: Vec<Vec<u8>>,
    bindings: Vec<Vec<u8>>,
    entry: u8,
) -> Vec<u8> {
    array([
        text("TPSTG"),
        uint(SCHEMA_VERSION),
        text("ghc-9.12-prepared-stg"),
        text("ghc-9.12.2"),
        uint(EXECUTION_ABI_VERSION),
        array([
            uint(0),
            uint(0),
            uint(64),
            uint(64),
            text("sysv64"),
            array([]),
        ]),
        array(signatures),
        array([]),
        array(constructors),
        array([]),
        array(expressions),
        array(bindings),
        uint(u64::from(entry)),
    ])
}

fn wire_program(
    signatures: Vec<Vec<u8>>,
    constructors: Vec<Vec<u8>>,
    expressions: Vec<Vec<u8>>,
    top_body: u8,
) -> Vec<u8> {
    wire_program_with_bindings(
        signatures,
        constructors,
        expressions,
        vec![group_nonrecursive(top_binding(function_rhs(
            0,
            vec![],
            top_body,
        )))],
        0,
    )
}

fn case_wire(kind: u8) -> Vec<u8> {
    match kind {
        0 => wire_program(
            vec![signature(&[RuntimeRep::LiftedRef])],
            vec![constructor_decl()],
            vec![
                return_frame(vec![atom_ref(1)]),
                construct_frame(0),
                case_frame(
                    1,
                    1,
                    &[RuntimeRep::LiftedRef],
                    0,
                    vec![alternative(constructor_pattern(0), vec![], 0)],
                ),
            ],
            2,
        ),
        1 | 3 => wire_program(
            vec![signature(&[RuntimeRep::Int(64)])],
            vec![],
            vec![
                return_frame(vec![atom_scalar(scalar_int(7))]),
                return_frame(vec![atom_scalar(scalar_int(7))]),
                case_frame(
                    1,
                    1,
                    &[RuntimeRep::Int(64)],
                    kind,
                    vec![alternative(default_pattern(), vec![], 0)],
                ),
            ],
            2,
        ),
        2 => wire_program(
            vec![signature(&[RuntimeRep::Int(64), RuntimeRep::Word(64)])],
            vec![],
            vec![
                return_frame(vec![atom_ref(2), atom_ref(3)]),
                return_frame(vec![
                    atom_scalar(scalar_int(7)),
                    atom_scalar(scalar_word(9)),
                ]),
                case_frame(
                    1,
                    1,
                    &[RuntimeRep::Int(64), RuntimeRep::Word(64)],
                    2,
                    vec![alternative(default_pattern(), vec![2, 3], 0)],
                ),
            ],
            2,
        ),
        _ => panic!("unknown case kind {kind}"),
    }
}

fn primitive_default_first_wire(scrutinee: u64) -> Vec<u8> {
    wire_program(
        vec![signature(&[RuntimeRep::Int(64)])],
        vec![],
        vec![
            return_frame(vec![atom_scalar(scalar_int(11))]),
            return_frame(vec![atom_scalar(scalar_int(7))]),
            return_frame(vec![atom_scalar(scalar_int(scrutinee))]),
            case_frame(
                2,
                1,
                &[RuntimeRep::Int(64)],
                1,
                vec![
                    alternative(default_pattern(), vec![], 0),
                    alternative(literal_pattern(scalar_int(7)), vec![], 1),
                ],
            ),
        ],
        3,
    )
}

fn primitive_float_default_first_wire(scrutinee: u64) -> Vec<u8> {
    let float = RuntimeRep::Float(64);
    wire_program(
        vec![signature(&[float])],
        vec![],
        vec![
            return_frame(vec![atom_scalar(scalar_float64(2.0_f64.to_bits()))]),
            return_frame(vec![atom_scalar(scalar_float64(1.0_f64.to_bits()))]),
            return_frame(vec![atom_scalar(scalar_float64(scrutinee))]),
            array([
                uint(5),
                uint(2),
                uint(1),
                array([rep(float)]),
                array([uint(1), rep(float)]),
                array([
                    alternative(default_pattern(), vec![], 0),
                    alternative(
                        literal_pattern(scalar_float64(0.0_f64.to_bits())),
                        vec![],
                        1,
                    ),
                ]),
            ]),
        ],
        3,
    )
}

fn nested_invalid_enter_wire() -> Vec<u8> {
    wire_program_with_bindings(
        vec![signature(&[RuntimeRep::Int(64)])],
        vec![],
        vec![
            enter_frame(1, 0),
            return_frame(vec![atom_scalar(scalar_int(7))]),
            let_recursive_frame(
                vec![heap_binding(1, function_rhs(0, vec![value_ref_local(1)], 0))],
                1,
            ),
        ],
        vec![group_nonrecursive(top_binding(function_rhs(0, vec![], 2)))],
        0,
    )
}

fn static_constructor_enter_wire() -> Vec<u8> {
    wire_program_with_bindings(
        vec![signature(&[RuntimeRep::LiftedRef])],
        vec![constructor_decl()],
        vec![enter_frame(1, 0)],
        vec![
            group_nonrecursive(top_binding(function_rhs(0, vec![], 0))),
            group_nonrecursive(top_binding_named(1, "value", constructor_rhs())),
        ],
        0,
    )
}

fn join_wire() -> Vec<u8> {
    wire_program(
        vec![
            signature(&[RuntimeRep::Int(64)]),
            signature_with(&[RuntimeRep::Int(64)], &[RuntimeRep::Int(64)]),
        ],
        vec![],
        vec![
            return_frame(vec![atom_ref(1)]),
            jump_frame(0, vec![atom_scalar(scalar_int(7))]),
            let_joins_frame(join_binding(0, 1, 1, 0), 1),
        ],
        2,
    )
}

fn zero_result_wire() -> Vec<u8> {
    wire_program(vec![signature(&[])], vec![], vec![return_frame(vec![])], 0)
}

fn cyclic_static_top_wire() -> Vec<u8> {
    wire_program_with_bindings(
        vec![],
        vec![lifted_constructor_decl()],
        vec![],
        vec![group_recursive(vec![top_binding_named(
            0,
            "cycle",
            constructor_rhs_fields(0, vec![atom_ref(0)]),
        )])],
        0,
    )
}

fn finite_static_child_nursery_wire() -> Vec<u8> {
    wire_program_with_bindings(
        vec![signature(&[RuntimeRep::LiftedRef])],
        vec![constructor_decl(), lifted_constructor_decl()],
        vec![
            return_frame(vec![atom_ref(3)]),
            let_frame(
                heap_binding(2, constructor_rhs_fields(1, vec![atom_ref(1)])),
                0,
            ),
            let_frame(
                heap_binding(3, constructor_rhs_fields(1, vec![atom_ref(1)])),
                1,
            ),
        ],
        vec![
            group_nonrecursive(top_binding(function_rhs(0, vec![], 2))),
            group_nonrecursive(top_binding_named(
                1,
                "static_leaf",
                constructor_rhs_fields(0, vec![]),
            )),
        ],
        0,
    )
}

fn mixed_result_wire(binding_count: u8) -> Vec<u8> {
    let results = [
        RuntimeRep::LiftedRef,
        RuntimeRep::Int(64),
        RuntimeRep::LiftedRef,
        RuntimeRep::Word(64),
    ];
    let mut expressions = vec![return_frame(vec![
        atom_ref(1),
        atom_scalar(scalar_int(7)),
        atom_ref(binding_count),
        atom_scalar(scalar_word(9)),
    ])];
    for id in 2..=binding_count {
        let body = expressions.len() as u8 - 1;
        expressions.push(let_frame(heap_binding(id, constructor_rhs()), body));
    }
    let callee_body = binding_count - 1;
    let call_node = binding_count;
    expressions.push(call_frame(41, 1));
    let case_return_node = call_node + 1;
    expressions.push(return_frame(vec![
        atom_ref(50),
        atom_ref(51),
        atom_ref(52),
        atom_ref(53),
    ]));
    let mut post_case_body = case_return_node;
    for id in 60..=75 {
        expressions.push(let_frame(
            heap_binding(id, constructor_rhs()),
            post_case_body,
        ));
        post_case_body = expressions.len() as u8 - 1;
    }
    let case_node = post_case_body + 1;
    expressions.push(case_frame(
        call_node,
        49,
        &results,
        2,
        vec![alternative(
            default_pattern(),
            vec![50, 51, 52, 53],
            post_case_body,
        )],
    ));
    expressions.push(let_recursive_frame(
        vec![
            heap_binding(1, constructor_rhs()),
            heap_binding(41, function_rhs(1, vec![value_ref_local(1)], callee_body)),
        ],
        case_node,
    ));
    let bindings = vec![group_nonrecursive(top_binding(function_rhs(
        0,
        vec![],
        case_node + 1,
    )))];
    array([
        text("TPSTG"),
        uint(SCHEMA_VERSION),
        text("ghc-9.12-prepared-stg"),
        text("ghc-9.12.2"),
        uint(EXECUTION_ABI_VERSION),
        array([
            uint(0),
            uint(0),
            uint(64),
            uint(64),
            text("sysv64"),
            array([]),
        ]),
        array([signature(&results), signature(&results)]),
        array([]),
        array([constructor_decl()]),
        array([]),
        array(expressions),
        array(bindings),
        uint(0),
    ])
}

fn requirements() -> ProgramRequirements {
    ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: vec![],
        },
    }
}

fn linked_mixed_result_fixture() -> tidepool_repr::execution_schema::LinkedProgram {
    linked_wire(mixed_result_wire(40))
}

fn direct_call_arity(function: &ir::Function, inst: ir::Inst) -> Option<usize> {
    match &function.dfg.insts[inst] {
        InstructionData::Call { args, .. } => Some(args.len(&function.dfg.value_lists)),
        _ => None,
    }
}

fn first_reserve_call(function: &ir::Function) -> (ir::Inst, ir::Block) {
    function
        .layout
        .blocks()
        .find_map(|block| {
            function.layout.block_insts(block).find_map(|inst| {
                (direct_call_arity(function, inst) == Some(2)).then_some((inst, block))
            })
        })
        .expect("prepared allocation reserve call is present")
}

fn reserve_extent(function: &ir::Function, call: ir::Inst) -> u64 {
    let InstructionData::Call { args, .. } = &function.dfg.insts[call] else {
        panic!("reserve marker is not a direct call")
    };
    let extent = args.as_slice(&function.dfg.value_lists)[1];
    let ValueDef::Result(defining, 0) = function.dfg.value_def(extent) else {
        panic!("reserve extent is not an immediate")
    };
    let InstructionData::UnaryImm { imm, .. } = &function.dfg.insts[defining] else {
        panic!("reserve extent is not an integer constant")
    };
    u64::try_from(i64::from(*imm)).expect("reserve extent is non-negative")
}

fn call_count(function: &ir::Function, block: ir::Block) -> usize {
    function
        .layout
        .block_insts(block)
        .filter(|inst| function.dfg.insts[*inst].opcode().is_call())
        .count()
}

fn linked_wire(bytes: Vec<u8>) -> tidepool_repr::execution_schema::LinkedProgram {
    let prepared = parse_program(&bytes, &requirements(), DecodeLimits::default()).unwrap();
    link_program(
        prepared,
        &MachineImports {
            values: BTreeMap::new(),
        },
    )
    .unwrap()
}

#[test]
fn multivalue_managed_results_survive_collection_after_return() {
    let linked = linked_mixed_result_fixture();
    let program = CompiledProgram::compile(&linked).unwrap();
    let result = program
        .run_entry(
            tidepool_repr::execution_schema::ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 256,
                observation_budget: 10_000,
                collect_before_observation: true,
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(
        result.collections >= 2,
        "expected collection inside callee and after result-root registration, got {}",
        result.collections
    );
    assert_eq!(result.values.len(), 4);
    for index in [0, 2] {
        assert!(matches!(
            &result.values[index],
            Value::Con(identity, fields)
                if *identity == DataConId(100) && fields.is_empty()
        ));
    }
    assert!(matches!(
        &result.values[1],
        Value::Lit(Literal::LitInt(value)) if *value == 7
    ));
    assert!(matches!(
        &result.values[3],
        Value::Lit(Literal::LitWord(value)) if *value == 9
    ));
}

#[test]
fn returned_refs_survive_generated_collection_in_the_caller() {
    let program = CompiledProgram::compile(&linked_mixed_result_fixture()).unwrap();
    let result = program
        .run_entry(
            tidepool_repr::execution_schema::ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 256,
                observation_budget: 10_000,
                collect_before_observation: false,
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(
        result.collections >= 2,
        "expected generated collections in callee and caller, got {}",
        result.collections
    );
    assert_eq!(result.values.len(), 4);
    for index in [0, 2] {
        assert!(matches!(
            &result.values[index],
            Value::Con(identity, fields)
                if *identity == DataConId(100) && fields.is_empty()
        ));
    }
}

#[test]
fn connected_call_case_let_covers_every_admitted_case_classification() {
    for kind in 0..=3 {
        let linked = linked_wire(case_wire(kind));
        let program = CompiledProgram::compile(&linked).unwrap();
        let result = program
            .run_entry(
                tidepool_repr::execution_schema::ValueId(0),
                &[],
                &RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        match kind {
            0 => {
                assert_eq!(result.values.len(), 1);
                assert!(matches!(
                    &result.values[0],
                    Value::Con(identity, fields)
                        if *identity == DataConId(100) && fields.is_empty()
                ));
            }
            1 | 3 => {
                assert_eq!(result.values.len(), 1);
                assert!(matches!(
                    &result.values[0],
                    Value::Lit(Literal::LitInt(value)) if *value == 7
                ));
            }
            2 => {
                assert_eq!(result.values.len(), 2);
                assert!(matches!(
                    &result.values[0],
                    Value::Lit(Literal::LitInt(value)) if *value == 7
                ));
                assert!(matches!(
                    &result.values[1],
                    Value::Lit(Literal::LitWord(value)) if *value == 9
                ));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn primitive_case_checks_literals_after_a_default_in_source_order() {
    for (scrutinee, expected) in [(7, 7), (8, 11)] {
        let program =
            CompiledProgram::compile(&linked_wire(primitive_default_first_wire(scrutinee)))
                .unwrap();
        let result = program
            .run_entry(
                tidepool_repr::execution_schema::ValueId(0),
                &[],
                &RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(result.values.len(), 1);
        assert!(matches!(
            &result.values[0],
            Value::Lit(Literal::LitInt(value)) if *value == expected
        ));
    }
}

#[test]
fn primitive_float_case_uses_native_equality_after_a_default_in_source_order() {
    for (scrutinee, expected) in [
        ((-0.0_f64).to_bits(), 1.0_f64.to_bits()),
        (f64::NAN.to_bits(), 2.0_f64.to_bits()),
    ] {
        let program =
            CompiledProgram::compile(&linked_wire(primitive_float_default_first_wire(scrutinee)))
                .unwrap();
        let result = program
            .run_entry(
                tidepool_repr::execution_schema::ValueId(0),
                &[],
                &RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(matches!(
            &result.values[..],
            [Value::Lit(Literal::LitDouble(value))] if *value == expected
        ));
    }
}

#[test]
fn nested_function_rejection_reports_the_nested_expression_owner() {
    assert!(matches!(
        CompiledProgram::compile(&linked_wire(nested_invalid_enter_wire())),
        Err(CompileError::Unsupported(Unsupported::Expression {
            binding: tidepool_repr::execution_schema::ValueId(1),
            node: 0,
        }))
    ));
}

#[test]
fn connected_join_jump_returns_zero_effect_result() {
    let program = CompiledProgram::compile(&linked_wire(join_wire())).unwrap();
    let result = program
        .run_entry(
            tidepool_repr::execution_schema::ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(result.values.len(), 1);
    assert!(matches!(
        &result.values[0],
        Value::Lit(Literal::LitInt(value)) if *value == 7
    ));
}

#[test]
fn zero_result_entry_returns_no_observable_payload() {
    let program = CompiledProgram::compile(&linked_wire(zero_result_wire())).unwrap();
    let result = program
        .run_entry(
            tidepool_repr::execution_schema::ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(result.values.is_empty());
}

#[test]
fn observation_failure_does_not_expose_the_return_payload() {
    let program = CompiledProgram::compile(&linked_mixed_result_fixture()).unwrap();
    let error = program
        .run_entry(
            tidepool_repr::execution_schema::ValueId(0),
            &[],
            &RunOptions {
                observation_budget: 0,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ExecutionError::Observation(ObservationFailure::BudgetExceeded { limit: 0 })
    ));
}

#[test]
fn cyclic_static_top_terminates_with_typed_observation_budget_failure() {
    let program = CompiledProgram::compile(&linked_wire(cyclic_static_top_wire())).unwrap();
    let error = program
        .run_entry(
            tidepool_repr::execution_schema::ValueId(0),
            &[],
            &RunOptions {
                observation_budget: 3,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ExecutionError::Observation(ObservationFailure::BudgetExceeded { limit: 3 })
    ));
}

#[test]
fn nursery_allocation_gc_preserves_static_child_for_observation() {
    let program =
        CompiledProgram::compile(&linked_wire(finite_static_child_nursery_wire())).unwrap();
    let result = program
        .run_entry(
            tidepool_repr::execution_schema::ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes: 24,
                observation_budget: 10,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(
        result.collections >= 1,
        "expected the second nursery allocation to trigger GC"
    );
    assert!(matches!(
        result.values.as_slice(),
        [Value::Con(outer, fields)]
            if *outer == DataConId(101)
                && matches!(
                    fields.as_slice(),
                    [Value::Con(leaf, leaf_fields)]
                        if *leaf == DataConId(100) && leaf_fields.is_empty()
                )
    ));
}

#[test]
fn recursive_group_reserves_once_before_sibling_initialization() {
    let program = CompiledProgram::compile(&linked_mixed_result_fixture()).unwrap();
    let ir = program
        .pipeline
        .emitted_ir
        .as_ref()
        .expect("prepared compilation captures pre-compile IR");
    let function_id = program.entries[&tidepool_repr::execution_schema::ValueId(0)].function;
    let function = ir.get(&function_id).expect("top entry IR is captured");
    let (reserve, slow) = first_reserve_call(function);
    assert_eq!(reserve_extent(function, reserve), {
        let constructor = program.descriptors.first().unwrap().allocation_extent();
        let recursive_function = program.descriptors.last().unwrap().allocation_extent();
        u64::from(constructor + recursive_function)
    });
    assert_eq!(
        call_count(function, slow),
        1,
        "reserve slow path calls GC once"
    );

    let reserve_branch = function
        .layout
        .blocks()
        .find_map(|block| {
            let inst = function.layout.last_inst(block)?;
            let InstructionData::Brif { blocks, .. } = &function.dfg.insts[inst] else {
                return None;
            };
            blocks
                .iter()
                .any(|target| target.block(&function.dfg.value_lists) == slow)
                .then_some((block, blocks))
        })
        .expect("reserve slow path has a conditional fast/slow predecessor");
    let fast = reserve_branch
        .1
        .iter()
        .find(|target| target.block(&function.dfg.value_lists) != slow)
        .expect("reserve branch has a fast successor")
        .block(&function.dfg.value_lists);
    assert_eq!(
        call_count(function, fast),
        0,
        "successful bump path has no host call"
    );
    let fast_jump = function
        .layout
        .last_inst(fast)
        .expect("fast path terminator");
    let continuation = match &function.dfg.insts[fast_jump] {
        InstructionData::Jump { destination, .. } => destination.block(&function.dfg.value_lists),
        other => panic!("fast path does not continue after bump: {other:?}"),
    };

    let mut headers = 0;
    for inst in function.layout.block_insts(continuation) {
        let data = &function.dfg.insts[inst];
        assert!(
            !data.opcode().is_call(),
            "header initialization crosses a host call"
        );
        if data.opcode() == Opcode::Store && data.load_store_offset() == Some(0) {
            headers += 1;
            if headers == 2 {
                break;
            }
        }
    }
    assert_eq!(
        headers, 2,
        "recursive group initializes both sibling headers"
    );
}

#[test]
fn enter_zero_tag_uses_one_slow_inspection_call_without_an_inline_header_chain() {
    let program = CompiledProgram::compile(&linked_wire(static_constructor_enter_wire())).unwrap();
    let ir = program
        .pipeline
        .emitted_ir
        .as_ref()
        .expect("prepared compilation captures pre-compile IR");
    let function_id = program.entries[&tidepool_repr::execution_schema::ValueId(0)].function;
    let function = ir.get(&function_id).expect("top entry IR is captured");
    let two_argument_calls = function
        .layout
        .blocks()
        .flat_map(|block| function.layout.block_insts(block))
        .filter(|inst| direct_call_arity(function, *inst) == Some(2))
        .count();
    assert_eq!(
        two_argument_calls, 1,
        "Enter emits one slow inspection call"
    );
    let loads = function
        .layout
        .blocks()
        .flat_map(|block| function.layout.block_insts(block))
        .filter(|inst| function.dfg.insts[*inst].opcode() == Opcode::Load)
        .count();
    assert_eq!(loads, 2, "Enter performs only the top-table loads inline");
}
