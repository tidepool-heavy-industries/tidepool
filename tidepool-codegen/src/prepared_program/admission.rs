use super::Unsupported;
use std::collections::BTreeMap;
use tidepool_repr::execution_schema::{
    Atom, ExprFrame, GlobalId, Group, HeapBinding, HeapRhs, LinkedProgram, OperationDecl,
    PreparedProgram, ResultContract, RuntimeRep, Signature, SignatureId, ValueId, ValueRef,
};

/// Whether one validated operation declaration has an exact native lowering.
pub fn supports_operation(declaration: &OperationDecl, signature: &Signature) -> bool {
    super::primitives::recognize_operation(declaration, signature).is_some()
        || super::lifetime::callback_signature(declaration, signature).is_some()
}

fn items<T>(group: &Group<T>) -> &[T] {
    match group {
        Group::NonRecursive(item) => std::slice::from_ref(item),
        Group::Recursive(items) => items,
    }
}

fn expression_owners(program: &PreparedProgram) -> Vec<Option<ValueId>> {
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
    admit_prepared(linked.prepared())
}

/// Closed admission does not require inventing import handles merely to reject
/// them. Artifact tooling and compiled owners share this checked boundary.
///
/// A global is admitted per-declaration: its representation must be
/// `LiftedRef` or `UnliftedRef` (the only reps [`super::emit`]'s tops-load
/// lowering and [`super::machine::PreparedMachine::install_program`]'s
/// handle-shape check know how to carry). Anything else (a raw scalar,
/// `Address`, ...) is rejected with the real [`GlobalId`] so the caller can
/// name the offending import; identity, signature and generation agreement
/// were already proven by [`tidepool_repr::execution_schema::link_program`]
/// before this program reached admission.
pub fn admit_prepared(program: &PreparedProgram) -> Result<(), Unsupported> {
    for (index, declaration) in program.globals().iter().enumerate() {
        if !matches!(
            declaration.rep,
            RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef
        ) {
            return Err(Unsupported::Global(GlobalId(index as u32)));
        }
    }
    let mut functions = BTreeMap::new();
    let mut admit_binding = |binding: &HeapBinding, _top: bool| -> Result<(), Unsupported> {
        match &binding.rhs {
            HeapRhs::Thunk { signature, .. } => {
                let checked = program
                    .signatures()
                    .get(signature.0 as usize)
                    .ok_or(Unsupported::ThunkSignature(binding.id))?;
                let supported_result = match &checked.results {
                    ResultContract::NoSuccess => true,
                    ResultContract::Returns(reps) => reps.as_slice() == [RuntimeRep::LiftedRef],
                };
                if !checked.arguments.is_empty() || !supported_result {
                    return Err(Unsupported::ThunkSignature(binding.id));
                }
                functions.insert(binding.id, *signature);
            }
            HeapRhs::Function { signature, .. } => {
                functions.insert(binding.id, *signature);
            }
            HeapRhs::Constructor { .. } | HeapRhs::Bytes(_) => {}
        }
        Ok(())
    };
    for group in program.bindings() {
        for top in items(group) {
            admit_binding(&top.binding, true)?;
        }
    }
    for frame in &program.expressions().nodes {
        if let ExprFrame::Let { bindings, .. } = frame {
            for binding in items(bindings) {
                admit_binding(binding, false)?;
            }
        }
    }

    let owners = expression_owners(program);
    let mut pap_fallback_cache: BTreeMap<SignatureId, bool> = BTreeMap::new();
    for (node, frame) in program.expressions().nodes.iter().enumerate() {
        let rejected = match frame {
            ExprFrame::Operation { operation, .. } => {
                match program
                    .operations()
                    .get(operation.0 as usize)
                    .and_then(|declaration| {
                        program
                            .signatures()
                            .get(declaration.signature.0 as usize)
                            .map(|signature| (declaration, signature))
                    }) {
                    Some((declaration, signature)) => {
                        if supports_operation(declaration, signature) {
                            false
                        } else {
                            return Err(Unsupported::Operation {
                                binding: owners[node].unwrap_or(program.entry()),
                                node,
                                identity: declaration.identity.clone(),
                                signature: signature.clone(),
                            });
                        }
                    }
                    None => true,
                }
            }
            ExprFrame::Call {
                callee, signature, ..
            } => {
                match callee {
                    Atom::Ref(ValueRef::Local(id)) => {
                        match program.signatures().get(signature.0 as usize) {
                            None => true,
                            Some(demand) => {
                                if let Some(entry) = functions
                                    .get(id)
                                    .and_then(|actual| program.signatures().get(actual.0 as usize))
                                {
                                    super::apply::classify(entry, 0, demand).is_none()
                                } else {
                                    // Case/let values may be PAPs. Their actual
                                    // descriptor remains a generated dispatch
                                    // check, but admission can prove that at least
                                    // one owned callable/pending arity has this ABI.
                                    // `functions` is fixed once admission begins,
                                    // so this predicate is stable per demanded
                                    // signature and worth caching across nodes.
                                    !*pap_fallback_cache.entry(*signature).or_insert_with(|| {
                                        functions.values().any(|actual| {
                                            program.signatures().get(actual.0 as usize).is_some_and(
                                                |entry| {
                                                    (0..entry.arguments.len()).any(|pending| {
                                                        super::apply::classify(
                                                            entry, pending, demand,
                                                        )
                                                        .is_some()
                                                    })
                                                },
                                            )
                                        })
                                    })
                                }
                            }
                        }
                    }
                    _ => true,
                }
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
            RuntimeRep::Float(bits) => array([uint(6), uint(u64::from(bits))]),
            other => panic!("unsupported fixture representation {other:?}"),
        }
    }
    fn sig(args: &[RuntimeRep], results: &[RuntimeRep]) -> Vec<u8> {
        array([
            array(args.iter().copied().map(rep)),
            array([uint(0), array(results.iter().copied().map(rep))]),
        ])
    }

    fn no_success_sig(args: &[RuntimeRep]) -> Vec<u8> {
        array([array(args.iter().copied().map(rep)), array([uint(1)])])
    }

    fn symbol(name: &str) -> Vec<u8> {
        array([
            text("fixture"),
            text("Admission"),
            text("value"),
            text(name),
            array([uint(0)]),
        ])
    }
    fn none() -> Vec<u8> {
        array([uint(0)])
    }
    fn global(name: &str) -> Vec<u8> {
        global_with_rep(name, RuntimeRep::LiftedRef)
    }
    fn global_with_rep(name: &str, rep_value: RuntimeRep) -> Vec<u8> {
        array([symbol(name), rep(rep_value), none(), boolean(false), none()])
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

    fn operation_with_args(id: u8, arguments: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        array([uint(3), uint(u64::from(id)), array(arguments)])
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
                    evaluated: global.required_evaluated,
                    generation: global.required_generation.unwrap_or(0),
                };
                (value.identity.clone(), value)
            })
            .collect::<BTreeMap<_, _>>();
        link_program(prepared, &MachineImports { values }).unwrap()
    }

    #[test]
    fn admission_rejects_non_lifted_thunks_before_execution() {
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
            Err(Unsupported::ThunkSignature(ValueId(1)))
        );
    }

    #[test]
    fn admission_accepts_nonsuccess_thunks_before_execution() {
        let bytes = wire(
            vec![
                no_success_sig(&[]),
                no_success_sig(&[RuntimeRep::LiftedRef]),
            ],
            vec![],
            vec![],
            vec![array([array([uint(0), text("raise#")]), uint(1)])],
            vec![operation_with_args(
                0,
                [atom_rubbish(RuntimeRep::LiftedRef)],
            )],
            vec![array([uint(0), top(0, thunk(0, 0))])],
        );
        assert_eq!(admit_program(&linked(bytes)), Ok(()));
    }

    /// S3 test (4): admission admits a `LiftedRef` global (replaces the old
    /// blanket-rejection semantics of `admission_rejects_globals_with_typed_id`).
    #[test]
    fn admission_admits_a_lifted_ref_global() {
        let bytes = wire(
            vec![sig(&[], &[RuntimeRep::Int(64)])],
            vec![global("imported")],
            vec![],
            vec![],
            vec![ret([atom_int(1)])],
            vec![array([uint(0), top(0, function(0, &[], 0))])],
        );
        assert_eq!(admit_program(&linked(bytes)), Ok(()));
    }

    /// S3 test (4): a global whose representation is neither `LiftedRef` nor
    /// `UnliftedRef` is rejected with the real `GlobalId`, unlike a supported
    /// import. `Float(64)` is the plan card's named example (this wave carries
    /// no lowering for a raw scalar/float import).
    #[test]
    fn admission_rejects_a_float_global_with_typed_id() {
        let bytes = wire(
            vec![sig(&[], &[RuntimeRep::Int(64)])],
            vec![global_with_rep("imported", RuntimeRep::Float(64))],
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
    fn admission_rejects_indirect_calls_and_accepts_partial_calls() {
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
        assert_eq!(admit_program(&linked(partial)), Ok(()));
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
    fn admission_reports_the_nested_binding_operation_identity_and_signature() {
        let bytes = wire(
            vec![sig(&[], &[RuntimeRep::Int(64)])],
            vec![],
            vec![],
            vec![array([array([uint(0), text("op")]), uint(0)])],
            vec![
                operation(0),
                ret([atom_int(1)]),
                let_frame(group_nonrecursive(1, function(0, &[], 0)), 1),
            ],
            vec![array([uint(0), top(0, function(0, &[], 2))])],
        );
        assert_eq!(
            admit_program(&linked(bytes)),
            Err(Unsupported::Operation {
                binding: ValueId(1),
                node: 0,
                identity: tidepool_repr::execution_schema::OperationIdentity::PrimOp("op".into()),
                signature: tidepool_repr::execution_schema::Signature {
                    arguments: vec![],
                    results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                        RuntimeRep::Int(64)
                    ]),
                },
            })
        );
    }
}
