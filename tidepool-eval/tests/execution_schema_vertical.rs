use tidepool_eval::execution::{
    execute_linked, NoReferenceOperations, ReferenceExecution, ReferenceImports, ReferenceValue,
};
use tidepool_repr::execution_schema::{
    link_program, parse_program, Architecture, DecodeLimits, Endianness, Expr, Group, HeapBinding,
    HeapRhs, ImportedValue, MachineImports, ProgramRequirements, ScalarLiteral, TargetDescriptor,
    TopBinding, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

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

#[test]
fn prepared_worker_bytes_link_and_execute() {
    let bytes = include_bytes!("fixtures/m3-vertical.cbor");
    let prepared = parse_program(bytes, &requirements(), DecodeLimits::default())
        .expect("candidate Haskell writer output must validate");
    assert!(
        !prepared.globals().is_empty(),
        "prepared worker omitted imported identities"
    );
    assert!(
        !prepared.constructors().is_empty(),
        "prepared worker omitted constructor layouts"
    );
    assert!(
        prepared.bindings().iter().any(top_group_has_recursion),
        "prepared worker omitted recursive bindings"
    );

    let values = prepared
        .globals()
        .iter()
        .map(|global| {
            let imported = ImportedValue {
                identity: global.identity.clone(),
                signature: prepared.signatures()[global.signature.0 as usize].clone(),
                evaluated: global.required_evaluated,
                generation: global.required_generation.unwrap_or(0),
            };
            (imported.identity.clone(), imported)
        })
        .collect();
    let linked = link_program(prepared, &MachineImports { values })
        .expect("candidate imports must link atomically");
    let mut operations = NoReferenceOperations;
    let result = execute_linked(
        &linked,
        &mut ReferenceExecution {
            imports: &ReferenceImports::default(),
            operations: &mut operations,
            fuel: 10_000,
        },
    )
    .expect("validated candidate must execute in the reference consumer");

    assert!(matches!(
        result.as_slice(),
        [ReferenceValue::Constructor { fields, .. }]
            if fields == &[ReferenceValue::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 42_i64.to_be_bytes().to_vec(),
            })]
    ));
}

fn top_group_has_recursion(group: &Group<TopBinding>) -> bool {
    matches!(group, Group::Recursive(_))
        || group_items(group)
            .iter()
            .any(|top| rhs_has_recursion(&top.binding.rhs))
}

fn heap_group_has_recursion(group: &Group<HeapBinding>) -> bool {
    matches!(group, Group::Recursive(_))
        || group_items(group)
            .iter()
            .any(|binding| rhs_has_recursion(&binding.rhs))
}

fn group_items<T>(group: &Group<T>) -> &[T] {
    match group {
        Group::NonRecursive(item) => std::slice::from_ref(item),
        Group::Recursive(items) => items,
    }
}

fn rhs_has_recursion(rhs: &HeapRhs) -> bool {
    match rhs {
        HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } => expr_has_recursion(body),
        HeapRhs::Constructor { .. } => false,
    }
}

fn expr_has_recursion(expression: &Expr) -> bool {
    match expression {
        Expr::LetJoins { .. } => true,
        Expr::Let { bindings, body } => {
            heap_group_has_recursion(bindings) || expr_has_recursion(body)
        }
        Expr::Case {
            scrutinee,
            alternatives,
            ..
        } => {
            expr_has_recursion(scrutinee)
                || alternatives
                    .iter()
                    .any(|alternative| expr_has_recursion(&alternative.body))
        }
        _ => false,
    }
}
