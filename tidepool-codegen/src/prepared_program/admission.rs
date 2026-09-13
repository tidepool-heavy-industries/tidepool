use super::Unsupported;
use std::collections::BTreeMap;
use tidepool_repr::execution_schema::{
    Atom, ExprFrame, GlobalId, Group, HeapBinding, HeapRhs, LinkedProgram, ValueId, ValueRef,
};

fn items<T>(group: &Group<T>) -> &[T] {
    match group {
        Group::NonRecursive(item) => std::slice::from_ref(item),
        Group::Recursive(items) => items,
    }
}

fn expression_owners(linked: &LinkedProgram) -> Vec<Option<ValueId>> {
    let program = linked.prepared();
    let mut owners = vec![None; program.expressions().nodes.len()];
    let mut pending = Vec::new();
    for group in program.bindings() {
        for top in items(group) {
            if let HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } = &top.binding.rhs {
                pending.push((*body, top.binding.id));
            }
        }
    }
    while let Some((node, owner)) = pending.pop() {
        if owners[node].is_some() {
            continue;
        }
        owners[node] = Some(owner);
        match &program.expressions().nodes[node] {
            ExprFrame::Case {
                scrutinee,
                alternatives,
                ..
            } => {
                pending.push((*scrutinee, owner));
                pending.extend(
                    alternatives
                        .iter()
                        .map(|alternative| (alternative.body, owner)),
                );
            }
            ExprFrame::Let { bindings, body } => {
                pending.push((*body, owner));
                for binding in items(bindings) {
                    if let HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } =
                        &binding.rhs
                    {
                        pending.push((*body, binding.id));
                    }
                }
            }
            ExprFrame::LetJoins { bindings, body } => {
                pending.push((*body, owner));
                pending.extend(items(bindings).iter().map(|binding| (binding.body, owner)));
            }
            ExprFrame::Return(_)
            | ExprFrame::Enter { .. }
            | ExprFrame::Call { .. }
            | ExprFrame::Operation { .. }
            | ExprFrame::Construct { .. }
            | ExprFrame::Jump { .. } => {}
        }
    }
    owners
}

/// Admission is whole-program and precedes declaration/publication. Validation
/// already proved the flat arena's ownership, bounds, scopes and representations.
pub fn admit_program(linked: &LinkedProgram) -> Result<(), Unsupported> {
    let program = linked.prepared();
    if !program.globals().is_empty() {
        return Err(Unsupported::Global(GlobalId(0)));
    }
    let mut functions = BTreeMap::new();
    let mut admit_binding = |binding: &HeapBinding| -> Result<(), Unsupported> {
        match &binding.rhs {
            HeapRhs::Thunk { .. } => return Err(Unsupported::Thunk(binding.id)),
            HeapRhs::Function { signature, .. } => {
                functions.insert(binding.id, *signature);
            }
            HeapRhs::Constructor { .. } | HeapRhs::Bytes(_) => {}
        }
        Ok(())
    };
    for group in program.bindings() {
        for top in items(group) {
            admit_binding(&top.binding)?;
        }
    }
    for frame in &program.expressions().nodes {
        if let ExprFrame::Let { bindings, .. } = frame {
            for binding in items(bindings) {
                admit_binding(binding)?;
            }
        }
    }

    let owners = expression_owners(linked);
    for (node, frame) in program.expressions().nodes.iter().enumerate() {
        let rejected = match frame {
            ExprFrame::Operation { .. } => true,
            ExprFrame::Call {
                callee, signature, ..
            } => {
                let actual = match callee {
                    Atom::Ref(ValueRef::Local(id)) => functions.get(id),
                    _ => None,
                };
                actual.is_none_or(|actual| {
                    program.signatures()[actual.0 as usize]
                        != program.signatures()[signature.0 as usize]
                })
            }
            _ => false,
        };
        if rejected {
            return Err(Unsupported::Expression {
                binding: owners[node].unwrap_or(program.entry()),
                node,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tidepool_repr::execution_schema::{
        link_program, parse_program, Architecture, DecodeLimits, Endianness, ImportedValue,
        MachineImports, ProgramRequirements, RuntimeRep, TargetDescriptor, EXECUTION_ABI_VERSION,
        SCHEMA_VERSION,
    };

    fn head(major: u8, length: usize) -> Vec<u8> {
        assert!(length < 24);
        vec![(major << 5) | length as u8]
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
    fn boolean(value: bool) -> Vec<u8> {
        vec![if value { 0xf5 } else { 0xf4 }]
    }
    fn rep(rep: RuntimeRep) -> Vec<u8> {
        match rep {
            RuntimeRep::Void => array([uint(0)]),
            RuntimeRep::LiftedRef => array([uint(1)]),
            RuntimeRep::Int(bits) => array([uint(4), uint(u64::from(bits))]),
            RuntimeRep::Word(bits) => array([uint(5), uint(u64::from(bits))]),
            other => panic!("unsupported fixture representation {other:?}"),
        }
    }
    fn sig(args: &[RuntimeRep], results: &[RuntimeRep]) -> Vec<u8> {
        array([
            array(args.iter().copied().map(rep)),
            array(results.iter().copied().map(rep)),
        ])
    }
    fn symbol(name: &str) -> Vec<u8> {
        array([
            text("fixture"),
            text("Admission"),
            text("value"),
            text(name),
        ])
    }
    fn none() -> Vec<u8> {
        array([uint(0)])
    }
    fn global(name: &str) -> Vec<u8> {
        array([
            symbol(name),
            rep(RuntimeRep::LiftedRef),
            none(),
            boolean(false),
            none(),
            boolean(false),
        ])
    }
    fn scalar_int(value: u8) -> Vec<u8> {
        array([uint(0), uint(64), bytes(&[value, 0, 0, 0, 0, 0, 0, 0])])
    }
    fn atom_int(value: u8) -> Vec<u8> {
        array([uint(1), scalar_int(value)])
    }
    fn atom_local(id: u8) -> Vec<u8> {
        array([uint(0), array([uint(0), uint(u64::from(id))])])
    }
    fn atom_rubbish(value: RuntimeRep) -> Vec<u8> {
        array([uint(3), rep(value)])
    }
    fn ret(atoms: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        array([uint(0), array(atoms)])
    }
    fn call(callee: Vec<u8>, signature: u8, args: Vec<Vec<u8>>) -> Vec<u8> {
        array([uint(2), callee, uint(u64::from(signature)), array(args)])
    }
    fn operation(id: u8) -> Vec<u8> {
        array([uint(3), uint(u64::from(id)), array([])])
    }
    fn let_frame(bindings: Vec<u8>, body: u8) -> Vec<u8> {
        array([uint(6), bindings, uint(u64::from(body))])
    }
    fn group_nonrecursive(id: u8, rhs: Vec<u8>) -> Vec<u8> {
        array([uint(0), heap_binding(id, rhs)])
    }
    fn heap_binding(id: u8, rhs: Vec<u8>) -> Vec<u8> {
        array([uint(u64::from(id)), rhs])
    }
    fn group_recursive(bindings: Vec<Vec<u8>>) -> Vec<u8> {
        array([uint(1), array(bindings)])
    }
    fn function(signature: u8, params: &[u8], body: u8) -> Vec<u8> {
        array([
            uint(0),
            uint(u64::from(signature)),
            array(params.iter().copied().map(|id| uint(u64::from(id)))),
            array([]),
            uint(u64::from(body)),
        ])
    }
    fn thunk(signature: u8, body: u8) -> Vec<u8> {
        array([
            uint(1),
            uint(u64::from(signature)),
            uint(0),
            array([]),
            uint(u64::from(body)),
        ])
    }
    fn constructor_rhs() -> Vec<u8> {
        array([uint(2), uint(0), array([])])
    }
    fn top(id: u8, rhs: Vec<u8>) -> Vec<u8> {
        array([symbol("entry"), array([uint(u64::from(id)), rhs])])
    }
    fn constructor() -> Vec<u8> {
        array([
            symbol("Box"),
            symbol("BoxFamily"),
            array([]),
            array([]),
            array([array([]), uint(1), uint(0), array([])]),
            rep(RuntimeRep::LiftedRef),
            uint(1),
            uint(1),
            uint(1),
        ])
    }
    fn wire(
        signatures: Vec<Vec<u8>>,
        globals: Vec<Vec<u8>>,
        constructors: Vec<Vec<u8>>,
        operations: Vec<Vec<u8>>,
        expressions: Vec<Vec<u8>>,
        bindings: Vec<Vec<u8>>,
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
            array(globals),
            array(constructors),
            array(operations),
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
    fn linked(bytes: Vec<u8>) -> LinkedProgram {
        let prepared = parse_program(&bytes, &requirements(), DecodeLimits::default()).unwrap();
        let values = prepared
            .globals()
            .iter()
            .map(|global| {
                let value = ImportedValue {
                    identity: global.identity.clone(),
                    rep: global.rep,
                    entry_signature: global
                        .entry_signature
                        .map(|id| prepared.signatures()[id.0 as usize].clone()),
                    dead_end: global.dead_end,
                    evaluated: global.required_evaluated,
                    generation: global.required_generation.unwrap_or(0),
                };
                (value.identity.clone(), value)
            })
            .collect::<BTreeMap<_, _>>();
        link_program(prepared, &MachineImports { values }).unwrap()
    }

    #[test]
    fn admission_rejects_nested_thunks_before_execution() {
        let bytes = wire(
            vec![sig(&[], &[RuntimeRep::Int(64)])],
            vec![],
            vec![],
            vec![],
            vec![
                ret([atom_int(1)]),
                ret([atom_int(2)]),
                let_frame(group_nonrecursive(1, thunk(0, 0)), 1),
            ],
            vec![array([uint(0), top(0, function(0, &[], 2))])],
        );
        assert_eq!(
            admit_program(&linked(bytes)),
            Err(Unsupported::Thunk(ValueId(1)))
        );
    }

    #[test]
    fn admission_rejects_globals_with_typed_id() {
        let bytes = wire(
            vec![sig(&[], &[RuntimeRep::Int(64)])],
            vec![global("imported")],
            vec![],
            vec![],
            vec![ret([atom_int(1)])],
            vec![array([uint(0), top(0, function(0, &[], 0))])],
        );
        assert_eq!(
            admit_program(&linked(bytes)),
            Err(Unsupported::Global(GlobalId(0)))
        );
    }

    #[test]
    fn admission_rejects_indirect_and_partial_calls() {
        let indirect = wire(
            vec![
                sig(&[RuntimeRep::LiftedRef], &[RuntimeRep::Int(64)]),
                sig(&[], &[RuntimeRep::Int(64)]),
            ],
            vec![],
            vec![],
            vec![],
            vec![call(atom_local(1), 1, vec![])],
            vec![array([uint(0), top(0, function(0, &[1], 0))])],
        );
        assert_eq!(
            admit_program(&linked(indirect)),
            Err(Unsupported::Expression {
                binding: ValueId(0),
                node: 0
            })
        );

        let partial = wire(
            vec![
                sig(&[RuntimeRep::Int(64)], &[RuntimeRep::LiftedRef]),
                sig(&[], &[RuntimeRep::LiftedRef]),
            ],
            vec![],
            vec![],
            vec![],
            vec![
                ret([atom_rubbish(RuntimeRep::LiftedRef)]),
                call(atom_local(1), 1, vec![]),
                let_frame(group_nonrecursive(1, function(0, &[2], 0)), 1),
            ],
            vec![array([uint(0), top(0, function(1, &[], 2))])],
        );
        assert_eq!(
            admit_program(&linked(partial)),
            Err(Unsupported::Expression {
                binding: ValueId(0),
                node: 1
            })
        );
    }

    #[test]
    fn admission_accepts_closed_constructor_and_function_lets() {
        let bytes = wire(
            vec![
                sig(&[], &[RuntimeRep::LiftedRef]),
                sig(&[], &[RuntimeRep::LiftedRef]),
            ],
            vec![],
            vec![constructor()],
            vec![],
            vec![
                ret([atom_rubbish(RuntimeRep::LiftedRef)]),
                ret([atom_local(1)]),
                let_frame(
                    group_recursive(vec![
                        heap_binding(1, constructor_rhs()),
                        heap_binding(2, function(1, &[], 0)),
                    ]),
                    1,
                ),
            ],
            vec![array([uint(0), top(0, function(0, &[], 2))])],
        );
        assert_eq!(admit_program(&linked(bytes)), Ok(()));
    }

    #[test]
    fn admission_accepts_an_exact_direct_call() {
        let bytes = wire(
            vec![
                sig(&[], &[RuntimeRep::Int(64)]),
                sig(&[RuntimeRep::Int(64)], &[RuntimeRep::Int(64)]),
            ],
            vec![],
            vec![],
            vec![],
            vec![
                ret([atom_int(1)]),
                call(atom_local(1), 1, vec![atom_int(2)]),
                let_frame(group_nonrecursive(1, function(1, &[2], 0)), 1),
            ],
            vec![array([uint(0), top(0, function(0, &[], 2))])],
        );
        assert_eq!(admit_program(&linked(bytes)), Ok(()));
    }

    #[test]
    fn admission_reports_the_nested_binding_owning_an_operation() {
        let bytes = wire(
            vec![sig(&[], &[RuntimeRep::Int(64)])],
            vec![],
            vec![],
            vec![array([text("op"), uint(0)])],
            vec![
                operation(0),
                ret([atom_int(1)]),
                let_frame(group_nonrecursive(1, function(0, &[], 0)), 1),
            ],
            vec![array([uint(0), top(0, function(0, &[], 2))])],
        );
        assert_eq!(
            admit_program(&linked(bytes)),
            Err(Unsupported::Expression {
                binding: ValueId(1),
                node: 0
            })
        );
    }
}
