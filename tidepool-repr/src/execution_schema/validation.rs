use std::collections::{BTreeMap, BTreeSet};

use super::{
    Alternative, AlternativePattern, Atom, CheckedLayout, ConstructorId, DecodeLimits, Expr,
    GlobalId, Group, HeapBinding, HeapRhs, JoinBinding, JoinId, OperationId, ParseError,
    ProgramRequirements, RuntimeRep, ScalarLiteral, SignatureId, SymbolIdentity, ValueId, ValueRef,
    WireProgram, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};

#[derive(Clone, Copy)]
struct ValueType {
    rep: RuntimeRep,
    callable: Option<SignatureId>,
    components: usize,
}

type TypeEnv = BTreeMap<ValueId, ValueType>;

pub(super) fn validate_program(
    wire: &WireProgram,
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
) -> Result<(), ParseError> {
    Validator::new(wire, limits).validate(requirements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_schema::{
        Architecture, ConstructorDecl, Endianness, FieldLayout, HeapBinding, HeapRhs,
        OperationDecl, ProgramEnvelope, Signature, TargetDescriptor, TopBinding, UpdatePolicy,
        EXECUTION_ABI_VERSION, SCHEMA_VERSION,
    };

    fn symbol(name: &str) -> SymbolIdentity {
        SymbolIdentity {
            unit: "fixture".into(),
            module: "M3.Validation".into(),
            namespace: "value".into(),
            occurrence: name.into(),
        }
    }

    fn target() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: vec![],
        }
    }

    fn requirements() -> ProgramRequirements {
        ProgramRequirements {
            schema_version: SCHEMA_VERSION,
            projection_profile: "ghc-9.12-prepared-stg".into(),
            toolchain: "ghc-9.12.2".into(),
            execution_abi_version: EXECUTION_ABI_VERSION,
            target: target(),
        }
    }

    fn valid_program() -> WireProgram {
        WireProgram {
            envelope: ProgramEnvelope {
                schema_version: SCHEMA_VERSION,
                projection_profile: "ghc-9.12-prepared-stg".into(),
                toolchain: "ghc-9.12.2".into(),
                execution_abi_version: EXECUTION_ABI_VERSION,
                target: target(),
            },
            signatures: vec![Signature {
                arguments: vec![],
                results: vec![RuntimeRep::Int(64)],
            }],
            globals: vec![],
            constructors: vec![],
            operations: vec![],
            bindings: vec![Group::NonRecursive(TopBinding {
                identity: symbol("entry"),
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: Box::new(Expr::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                            bits: 64,
                            bytes: 42_i64.to_be_bytes().to_vec(),
                        })])),
                    },
                },
            })],
            entry: ValueId(0),
        }
    }

    #[test]
    fn accepts_representative_valid_program() {
        validate_program(&valid_program(), &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn rejects_out_of_scope_value_without_publishing() {
        let mut program = valid_program();
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        **body = Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(99)))]);
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(_))
        ));
    }

    #[test]
    fn rejects_duplicate_top_level_value_id() {
        let mut program = valid_program();
        let Group::NonRecursive(binding) = program.bindings[0].clone() else {
            unreachable!()
        };
        program.bindings.push(Group::NonRecursive(TopBinding {
            identity: symbol("other"),
            binding: binding.binding,
        }));
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition(_))
        ));
    }

    #[test]
    fn rejects_nested_closure_with_missing_capture() {
        let mut program = valid_program();
        program.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        program.signatures.push(Signature {
            arguments: vec![],
            results: vec![RuntimeRep::Int(64)],
        });
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        binding.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(1)],
            captures: vec![],
            body: Box::new(Expr::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(1),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))])),
                    },
                }),
                body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(2)))])),
            }),
        };
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(_))
        ));
    }

    #[test]
    fn enforces_semantic_work_limit() {
        let limits = DecodeLimits {
            max_work: 1,
            ..DecodeLimits::default()
        };
        assert!(matches!(
            validate_program(&valid_program(), &requirements(), limits),
            Err(ParseError::LimitExceeded("work"))
        ));
    }

    #[test]
    fn rejects_stale_schema_even_if_caller_requests_it() {
        let mut program = valid_program();
        program.envelope.schema_version = 99;
        let mut stale_requirements = requirements();
        stale_requirements.schema_version = 99;
        assert!(matches!(
            validate_program(&program, &stale_requirements, DecodeLimits::default()),
            Err(ParseError::UnsupportedVersion(99))
        ));
    }

    fn assert_invalid_signature(program: WireProgram) {
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidSignature(_))
        ));
    }

    #[test]
    fn rejects_function_body_with_wrong_result_representation() {
        let mut program = valid_program();
        program.signatures[0].results = vec![RuntimeRep::LiftedRef];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        binding.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: Box::new(Expr::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 42_i64.to_be_bytes().to_vec(),
            })])),
        };
        assert_invalid_signature(program);
    }

    #[test]
    fn rejects_call_argument_with_wrong_representation() {
        let mut program = valid_program();
        program.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        let Group::NonRecursive(callee) = &mut program.bindings[0] else {
            unreachable!()
        };
        callee.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(1)],
            captures: vec![],
            body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))])),
        };
        program.bindings.push(Group::NonRecursive(TopBinding {
            identity: symbol("caller"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: Box::new(Expr::Call {
                        signature: SignatureId(0),
                        callee: Atom::Ref(ValueRef::Local(ValueId(0))),
                        arguments: vec![Atom::Scalar(ScalarLiteral::Word {
                            bits: 64,
                            bytes: 1_u64.to_be_bytes().to_vec(),
                        })],
                    }),
                },
            },
        }));
        assert_invalid_signature(program);
    }

    #[test]
    fn rejects_captured_raw_value_used_as_wrong_result_representation() {
        let mut program = valid_program();
        program.signatures = vec![
            Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results: vec![RuntimeRep::LiftedRef],
            },
            Signature {
                arguments: vec![],
                results: vec![RuntimeRep::LiftedRef],
            },
        ];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        binding.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(1)],
            captures: vec![],
            body: Box::new(Expr::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: vec![],
                        captures: vec![ValueRef::Local(ValueId(1))],
                        body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))])),
                    },
                }),
                body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(2)))])),
            }),
        };
        assert_invalid_signature(program);
    }

    #[test]
    fn rejects_jump_and_operation_argument_representation_mismatches() {
        let wrong = Atom::Scalar(ScalarLiteral::Word {
            bits: 64,
            bytes: 1_u64.to_be_bytes().to_vec(),
        });
        let mut operation = valid_program();
        operation.operations.push(OperationDecl {
            identity: "op".into(),
            signature: SignatureId(0),
        });
        operation.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        let Group::NonRecursive(binding) = &mut operation.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        **body = Expr::Operation {
            operation: OperationId(0),
            arguments: vec![wrong.clone()],
        };
        assert_invalid_signature(operation);

        let mut jump = valid_program();
        jump.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        let Group::NonRecursive(binding) = &mut jump.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        **body = Expr::LetJoins {
            bindings: Group::NonRecursive(JoinBinding {
                id: JoinId(0),
                signature: SignatureId(0),
                parameters: vec![ValueId(1)],
                body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))])),
            }),
            body: Box::new(Expr::Jump {
                join: JoinId(0),
                arguments: vec![wrong],
            }),
        };
        assert_invalid_signature(jump);
    }

    #[test]
    fn rejects_unnaturally_aligned_managed_layout() {
        let mut program = valid_program();
        program.constructors.push(ConstructorDecl {
            identity: symbol("C"),
            family: symbol("T"),
            field_reps: vec![RuntimeRep::Int(8), RuntimeRep::LiftedRef],
            strict_fields: vec![true, false],
            layout: CheckedLayout {
                fields: vec![
                    FieldLayout {
                        rep: RuntimeRep::Int(8),
                        offset: 0,
                    },
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 1,
                    },
                ],
                alignment: 8,
                payload_size: 16,
                root_mask: vec![false, true],
            },
        });
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
    }
}

struct Validator<'a> {
    wire: &'a WireProgram,
    limits: DecodeLimits,
    work: usize,
    nodes: usize,
    top_values: BTreeSet<ValueId>,
}

impl<'a> Validator<'a> {
    fn new(wire: &'a WireProgram, limits: DecodeLimits) -> Self {
        Self {
            wire,
            limits,
            work: 0,
            nodes: 0,
            top_values: BTreeSet::new(),
        }
    }

    fn validate(mut self, requirements: &ProgramRequirements) -> Result<(), ParseError> {
        self.check_envelope(requirements)?;
        self.check_table_len(self.wire.signatures.len())?;
        self.check_table_len(self.wire.globals.len())?;
        self.check_table_len(self.wire.constructors.len())?;
        self.check_table_len(self.wire.operations.len())?;
        self.check_table_len(self.wire.bindings.len())?;

        for signature in &self.wire.signatures {
            self.bump_work(signature.arguments.len() + signature.results.len() + 1)?;
            for rep in signature.arguments.iter().chain(&signature.results) {
                self.check_rep(*rep)?;
            }
        }

        let mut global_symbols = BTreeSet::new();
        for global in &self.wire.globals {
            self.bump_work(1)?;
            self.check_symbol(&global.identity)?;
            if !global_symbols.insert(global.identity.clone()) {
                return Err(ParseError::DuplicateDefinition(format!(
                    "global {:?}",
                    global.identity
                )));
            }
            self.check_signature(global.signature)?;
        }

        let mut constructor_symbols = BTreeSet::new();
        for constructor in &self.wire.constructors {
            self.bump_work(constructor.field_reps.len() + 1)?;
            self.check_symbol(&constructor.identity)?;
            self.check_symbol(&constructor.family)?;
            if !constructor_symbols.insert(constructor.identity.clone()) {
                return Err(ParseError::DuplicateDefinition(format!(
                    "constructor {:?}",
                    constructor.identity
                )));
            }
            if constructor.field_reps.len() != constructor.strict_fields.len() {
                return Err(ParseError::InvalidLayout(
                    "constructor field/strictness length mismatch".into(),
                ));
            }
            for rep in &constructor.field_reps {
                self.check_rep(*rep)?;
            }
            self.check_layout(&constructor.field_reps, &constructor.layout)?;
        }

        let mut operation_names = BTreeSet::new();
        for operation in &self.wire.operations {
            self.bump_work(1)?;
            self.check_text(&operation.identity)?;
            if !operation_names.insert(operation.identity.as_str()) {
                return Err(ParseError::DuplicateDefinition(format!(
                    "operation {}",
                    operation.identity
                )));
            }
            self.check_signature(operation.signature)?;
        }

        let mut top_symbols = BTreeSet::new();
        for group in &self.wire.bindings {
            match group {
                Group::NonRecursive(binding) => {
                    self.register_top(binding, &mut top_symbols)?;
                }
                Group::Recursive(bindings) => {
                    if bindings.is_empty() {
                        return Err(ParseError::Malformed(
                            "recursive top-level group is empty".into(),
                        ));
                    }
                    self.check_table_len(bindings.len())?;
                    for binding in bindings {
                        self.register_top(binding, &mut top_symbols)?;
                    }
                }
            }
        }
        if !self.top_values.contains(&self.wire.entry) {
            return Err(ParseError::InvalidReference(format!(
                "entry value {:?} is not a top-level binding",
                self.wire.entry
            )));
        }

        let mut available = BTreeSet::new();
        for group in &self.wire.bindings {
            match group {
                Group::NonRecursive(binding) => {
                    self.check_heap_binding(&binding.binding, &available, 0)?;
                    available.insert(binding.binding.id);
                }
                Group::Recursive(bindings) => {
                    let mut recursive_scope = available.clone();
                    recursive_scope.extend(bindings.iter().map(|binding| binding.binding.id));
                    for binding in bindings {
                        self.check_heap_binding(&binding.binding, &recursive_scope, 0)?;
                    }
                    available = recursive_scope;
                }
            }
        }
        self.check_typed_bindings()?;
        Ok(())
    }

    /// Establish the representation facts native consumers rely on. Scope is
    /// checked by the pass above; this pass keeps representation checking
    /// separate so no partially typed program can be published.
    fn check_typed_bindings(&mut self) -> Result<(), ParseError> {
        let mut available = TypeEnv::new();
        for group in &self.wire.bindings {
            match group {
                Group::NonRecursive(binding) => {
                    self.check_typed_heap_binding(&binding.binding, &available, 0)?;
                    available.insert(binding.binding.id, self.binding_type(&binding.binding));
                }
                Group::Recursive(bindings) => {
                    let mut recursive = available.clone();
                    recursive.extend(
                        bindings.iter().map(|binding| {
                            (binding.binding.id, self.binding_type(&binding.binding))
                        }),
                    );
                    for binding in bindings {
                        self.check_typed_heap_binding(&binding.binding, &recursive, 0)?;
                    }
                    available = recursive;
                }
            }
        }
        Ok(())
    }

    fn binding_type(&self, binding: &HeapBinding) -> ValueType {
        match binding.rhs {
            HeapRhs::Function { signature, .. } => ValueType {
                rep: RuntimeRep::LiftedRef,
                callable: Some(signature),
                components: 1,
            },
            HeapRhs::Thunk { signature, .. } => {
                let results = &self.wire.signatures[signature.0 as usize].results;
                ValueType {
                    rep: results.first().copied().unwrap_or(RuntimeRep::Void),
                    callable: Some(signature),
                    components: results.len(),
                }
            }
            HeapRhs::Constructor { .. } => ValueType {
                rep: RuntimeRep::LiftedRef,
                callable: None,
                components: 1,
            },
        }
    }

    fn check_typed_heap_binding(
        &mut self,
        binding: &HeapBinding,
        outer: &TypeEnv,
        depth: usize,
    ) -> Result<(), ParseError> {
        match &binding.rhs {
            HeapRhs::Function {
                signature,
                parameters,
                captures,
                body,
            } => {
                let signature = self.signature(*signature)?.clone();
                self.check_typed_captures(captures, outer)?;
                let mut scope = self.typed_closure_scope(outer, captures)?;
                scope.extend(
                    parameters
                        .iter()
                        .copied()
                        .zip(signature.arguments.iter().copied())
                        .map(|(id, rep)| {
                            (
                                id,
                                ValueType {
                                    rep,
                                    callable: None,
                                    components: 1,
                                },
                            )
                        }),
                );
                self.check_typed_expr(
                    body,
                    &scope,
                    &BTreeMap::new(),
                    Some(&signature.results),
                    depth + 1,
                )?;
            }
            HeapRhs::Thunk {
                signature,
                captures,
                body,
                ..
            } => {
                let signature = self.signature(*signature)?.clone();
                if !signature.arguments.is_empty() {
                    return Err(ParseError::InvalidSignature(
                        "thunk signature has arguments".into(),
                    ));
                }
                self.check_typed_captures(captures, outer)?;
                let scope = self.typed_closure_scope(outer, captures)?;
                self.check_typed_expr(
                    body,
                    &scope,
                    &BTreeMap::new(),
                    Some(&signature.results),
                    depth + 1,
                )?;
            }
            HeapRhs::Constructor {
                constructor,
                fields,
            } => {
                let reps = self.constructor(*constructor)?.field_reps.clone();
                self.check_typed_atoms(fields, outer, &reps, "constructor fields")?;
            }
        }
        Ok(())
    }

    fn check_typed_expr(
        &mut self,
        expression: &Expr,
        values: &TypeEnv,
        joins: &BTreeMap<JoinId, SignatureId>,
        expected: Option<&[RuntimeRep]>,
        depth: usize,
    ) -> Result<Vec<RuntimeRep>, ParseError> {
        self.bump_node(depth)?;
        let actual = match expression {
            Expr::Return(atoms) => self.typed_atom_reps(atoms, values)?,
            Expr::Enter { callee, signature } => self
                .check_atom_callable(callee, *signature, values)?
                .results
                .clone(),
            Expr::Call {
                callee,
                signature,
                arguments,
            } => {
                let signature = self
                    .check_atom_callable(callee, *signature, values)?
                    .clone();
                self.check_typed_atoms(arguments, values, &signature.arguments, "call arguments")?;
                signature.results
            }
            Expr::Operation {
                operation,
                arguments,
            } => {
                let signature = self
                    .signature(self.operation(*operation)?.signature)?
                    .clone();
                self.check_typed_atoms(
                    arguments,
                    values,
                    &signature.arguments,
                    "operation arguments",
                )?;
                signature.results
            }
            Expr::Construct {
                constructor,
                fields,
            } => {
                let reps = self.constructor(*constructor)?.field_reps.clone();
                self.check_typed_atoms(fields, values, &reps, "constructor fields")?;
                vec![RuntimeRep::LiftedRef]
            }
            Expr::Case {
                scrutinee,
                binder,
                results,
                alternatives,
            } => {
                self.check_typed_expr(scrutinee, values, joins, Some(results), depth + 1)?;
                let mut case_scope = values.clone();
                if let [rep] = results.as_slice() {
                    case_scope.insert(
                        *binder,
                        ValueType {
                            rep: *rep,
                            callable: None,
                            components: 1,
                        },
                    );
                }
                let mut result = None;
                for alternative in alternatives {
                    let mut scope = case_scope.clone();
                    if let AlternativePattern::Constructor(id) = alternative.pattern {
                        for (binder, rep) in alternative
                            .binders
                            .iter()
                            .zip(self.constructor(id)?.field_reps.iter().copied())
                        {
                            scope.insert(
                                *binder,
                                ValueType {
                                    rep,
                                    callable: None,
                                    components: 1,
                                },
                            );
                        }
                    }
                    let reps = self.check_typed_expr(
                        &alternative.body,
                        &scope,
                        joins,
                        expected,
                        depth + 1,
                    )?;
                    if result.as_ref().is_some_and(|prior| prior != &reps) {
                        return Err(ParseError::InvalidSignature(
                            "case alternatives return different representations".into(),
                        ));
                    }
                    result = Some(reps);
                }
                result.unwrap_or_default()
            }
            Expr::Let { bindings, body } => {
                let scope = self.check_typed_local_group(bindings, values, depth + 1)?;
                self.check_typed_expr(body, &scope, joins, expected, depth + 1)?
            }
            Expr::LetJoins { bindings, body } => {
                let join_scope = self.check_typed_join_group(bindings, values, joins, depth + 1)?;
                self.check_typed_expr(body, values, &join_scope, expected, depth + 1)?
            }
            Expr::Jump { join, arguments } => {
                let signature = self
                    .signature(*joins.get(join).ok_or_else(|| {
                        ParseError::InvalidScope(format!("join {:?} is out of scope", join))
                    })?)?
                    .clone();
                self.check_typed_atoms(arguments, values, &signature.arguments, "join arguments")?;
                signature.results
            }
        };
        if let Some(expected) = expected {
            if actual != expected {
                return Err(ParseError::InvalidSignature(format!(
                    "expression representations {actual:?} do not match expected {expected:?}"
                )));
            }
        }
        Ok(actual)
    }

    fn check_typed_local_group(
        &mut self,
        group: &Group<HeapBinding>,
        outer: &TypeEnv,
        depth: usize,
    ) -> Result<TypeEnv, ParseError> {
        let mut scope = outer.clone();
        match group {
            Group::NonRecursive(binding) => {
                self.check_typed_heap_binding(binding, outer, depth)?;
                scope.insert(binding.id, self.binding_type(binding));
            }
            Group::Recursive(bindings) => {
                scope.extend(
                    bindings
                        .iter()
                        .map(|binding| (binding.id, self.binding_type(binding))),
                );
                for binding in bindings {
                    self.check_typed_heap_binding(binding, &scope, depth)?;
                }
            }
        }
        Ok(scope)
    }

    fn check_typed_join_group(
        &mut self,
        group: &Group<JoinBinding>,
        values: &TypeEnv,
        outer: &BTreeMap<JoinId, SignatureId>,
        depth: usize,
    ) -> Result<BTreeMap<JoinId, SignatureId>, ParseError> {
        let mut scope = outer.clone();
        match group {
            Group::NonRecursive(binding) => {
                self.check_typed_join(binding, values, outer, depth)?;
                scope.insert(binding.id, binding.signature);
            }
            Group::Recursive(bindings) => {
                scope.extend(
                    bindings
                        .iter()
                        .map(|binding| (binding.id, binding.signature)),
                );
                for binding in bindings {
                    self.check_typed_join(binding, values, &scope, depth)?;
                }
            }
        }
        Ok(scope)
    }

    fn check_typed_join(
        &mut self,
        binding: &JoinBinding,
        values: &TypeEnv,
        joins: &BTreeMap<JoinId, SignatureId>,
        depth: usize,
    ) -> Result<(), ParseError> {
        let signature = self.signature(binding.signature)?.clone();
        let mut scope = values.clone();
        scope.extend(
            binding
                .parameters
                .iter()
                .copied()
                .zip(signature.arguments.iter().copied())
                .map(|(id, rep)| {
                    (
                        id,
                        ValueType {
                            rep,
                            callable: None,
                            components: 1,
                        },
                    )
                }),
        );
        self.check_typed_expr(
            &binding.body,
            &scope,
            joins,
            Some(&signature.results),
            depth + 1,
        )?;
        Ok(())
    }

    fn check_typed_captures(
        &self,
        captures: &[ValueRef],
        outer: &TypeEnv,
    ) -> Result<(), ParseError> {
        for capture in captures {
            self.value_ref_type(capture, outer)?;
        }
        Ok(())
    }

    fn typed_closure_scope(
        &self,
        outer: &TypeEnv,
        captures: &[ValueRef],
    ) -> Result<TypeEnv, ParseError> {
        let mut scope: TypeEnv = outer
            .iter()
            .filter(|(id, _)| self.top_values.contains(id))
            .map(|(id, ty)| (*id, *ty))
            .collect();
        for capture in captures {
            if let ValueRef::Local(id) = capture {
                scope.insert(*id, self.value_ref_type(capture, outer)?);
            }
        }
        Ok(scope)
    }

    fn typed_atom_reps(
        &mut self,
        atoms: &[Atom],
        values: &TypeEnv,
    ) -> Result<Vec<RuntimeRep>, ParseError> {
        atoms
            .iter()
            .map(|atom| self.atom_type(atom, values).map(|ty| ty.rep))
            .collect()
    }

    fn check_typed_atoms(
        &mut self,
        atoms: &[Atom],
        values: &TypeEnv,
        expected: &[RuntimeRep],
        context: &str,
    ) -> Result<(), ParseError> {
        let actual = self.typed_atom_reps(atoms, values)?;
        if actual != expected {
            return Err(ParseError::InvalidSignature(format!(
                "{context} {actual:?} do not match {expected:?}"
            )));
        }
        Ok(())
    }

    fn atom_type(&mut self, atom: &Atom, values: &TypeEnv) -> Result<ValueType, ParseError> {
        let rep = match atom {
            Atom::Ref(reference) => return self.value_ref_type(reference, values),
            Atom::Scalar(ScalarLiteral::Int { bits, .. }) => RuntimeRep::Int(*bits),
            Atom::Scalar(ScalarLiteral::Word { bits, .. }) => RuntimeRep::Word(*bits),
            Atom::Scalar(ScalarLiteral::Float { bits, .. }) => RuntimeRep::Float(*bits),
            Atom::Scalar(ScalarLiteral::Char(_)) => RuntimeRep::Word(32),
            Atom::Scalar(ScalarLiteral::Bytes(_)) => RuntimeRep::Address,
            Atom::Void => RuntimeRep::Void,
        };
        Ok(ValueType {
            rep,
            callable: None,
            components: 1,
        })
    }

    fn value_ref_type(
        &self,
        reference: &ValueRef,
        values: &TypeEnv,
    ) -> Result<ValueType, ParseError> {
        match reference {
            ValueRef::Local(id) => {
                let value = values.get(id).copied().ok_or_else(|| {
                    ParseError::InvalidScope(format!(
                        "value {:?} lacks representation evidence",
                        id
                    ))
                })?;
                if value.components != 1 {
                    return Err(ParseError::InvalidSignature(format!(
                        "value {:?} has {} representation components at an atomic use",
                        id, value.components
                    )));
                }
                Ok(value)
            }
            ValueRef::Global(id) => Ok(ValueType {
                rep: RuntimeRep::LiftedRef,
                callable: Some(self.global(*id)?.signature),
                components: 1,
            }),
        }
    }

    fn check_atom_callable<'b>(
        &'b mut self,
        atom: &Atom,
        declared: SignatureId,
        values: &TypeEnv,
    ) -> Result<&'b super::Signature, ParseError> {
        let declared_shape = self.signature(declared)?.clone();
        let ty = self.atom_type(atom, values)?;
        if let Some(actual) = ty.callable {
            if self.signature(actual)? != &declared_shape {
                return Err(ParseError::InvalidSignature(
                    "callee declaration does not match value signature".into(),
                ));
            }
        } else if ty.rep != RuntimeRep::LiftedRef
            && (!declared_shape.arguments.is_empty()
                || declared_shape.results.as_slice() != [ty.rep])
        {
            return Err(ParseError::InvalidSignature(
                "callee is not a callable reference".into(),
            ));
        }
        self.signature(declared)
    }

    fn register_top(
        &mut self,
        binding: &super::TopBinding,
        symbols: &mut BTreeSet<SymbolIdentity>,
    ) -> Result<(), ParseError> {
        self.bump_work(1)?;
        self.check_symbol(&binding.identity)?;
        if !symbols.insert(binding.identity.clone()) {
            return Err(ParseError::DuplicateDefinition(format!(
                "top-level symbol {:?}",
                binding.identity
            )));
        }
        if !self.top_values.insert(binding.binding.id) {
            return Err(ParseError::DuplicateDefinition(format!(
                "value id {:?}",
                binding.binding.id
            )));
        }
        Ok(())
    }

    fn check_heap_binding(
        &mut self,
        binding: &HeapBinding,
        outer: &BTreeSet<ValueId>,
        depth: usize,
    ) -> Result<(), ParseError> {
        self.bump_node(depth)?;
        match &binding.rhs {
            HeapRhs::Function {
                signature,
                parameters,
                captures,
                body,
            } => {
                let signature_value = self.signature(*signature)?;
                if signature_value.arguments.len() != parameters.len() {
                    return Err(ParseError::InvalidSignature(
                        "function parameter count does not match signature".into(),
                    ));
                }
                self.check_unique_values(parameters, "function parameter")?;
                self.check_captures(captures, outer)?;
                let mut scope = self.closure_scope(outer, captures);
                scope.extend(parameters.iter().copied());
                self.check_expr(body, &scope, &BTreeMap::new(), depth + 1)
            }
            HeapRhs::Thunk {
                signature,
                captures,
                body,
                ..
            } => {
                let signature = self.signature(*signature)?;
                if !signature.arguments.is_empty() {
                    return Err(ParseError::InvalidSignature(
                        "thunk signature has arguments".into(),
                    ));
                }
                self.check_captures(captures, outer)?;
                let scope = self.closure_scope(outer, captures);
                self.check_expr(body, &scope, &BTreeMap::new(), depth + 1)
            }
            HeapRhs::Constructor {
                constructor,
                fields,
            } => self.check_constructor_fields(*constructor, fields, outer),
        }
    }

    fn check_expr(
        &mut self,
        expression: &Expr,
        values: &BTreeSet<ValueId>,
        joins: &BTreeMap<JoinId, SignatureId>,
        depth: usize,
    ) -> Result<(), ParseError> {
        self.bump_node(depth)?;
        match expression {
            Expr::Return(atoms) => self.check_atoms(atoms, values),
            Expr::Enter { callee, signature } => {
                self.check_signature(*signature)?;
                self.check_atom(callee, values)
            }
            Expr::Call {
                callee,
                signature,
                arguments,
            } => {
                self.check_signature(*signature)?;
                self.check_atom(callee, values)?;
                self.check_atoms(arguments, values)
            }
            Expr::Operation {
                operation,
                arguments,
            } => {
                let declaration = self.operation(*operation)?;
                let signature = self.signature(declaration.signature)?;
                if signature.arguments.len() != arguments.len() {
                    return Err(ParseError::InvalidSignature(
                        "operation argument count does not match signature".into(),
                    ));
                }
                self.check_atoms(arguments, values)
            }
            Expr::Construct {
                constructor,
                fields,
            } => self.check_constructor_fields(*constructor, fields, values),
            Expr::Case {
                scrutinee,
                binder,
                results,
                alternatives,
            } => {
                self.check_expr(scrutinee, values, joins, depth + 1)?;
                for rep in results {
                    self.check_rep(*rep)?;
                }
                let mut scope = values.clone();
                if !scope.insert(*binder) {
                    return Err(ParseError::DuplicateDefinition(format!(
                        "case binder {:?}",
                        binder
                    )));
                }
                self.check_alternatives(alternatives, &scope, joins, depth + 1)
            }
            Expr::Let { bindings, body } => {
                let scope = self.check_local_group(bindings, values, depth + 1)?;
                self.check_expr(body, &scope, joins, depth + 1)
            }
            Expr::LetJoins { bindings, body } => {
                let join_scope = self.check_join_group(bindings, values, joins, depth + 1)?;
                self.check_expr(body, values, &join_scope, depth + 1)
            }
            Expr::Jump { join, arguments } => {
                let signature_id = joins.get(join).ok_or_else(|| {
                    ParseError::InvalidScope(format!("join {:?} is out of scope", join))
                })?;
                let signature = self.signature(*signature_id)?;
                if signature.arguments.len() != arguments.len() {
                    return Err(ParseError::InvalidSignature(
                        "jump argument count does not match signature".into(),
                    ));
                }
                self.check_atoms(arguments, values)
            }
        }
    }

    fn check_local_group(
        &mut self,
        group: &Group<HeapBinding>,
        outer: &BTreeSet<ValueId>,
        depth: usize,
    ) -> Result<BTreeSet<ValueId>, ParseError> {
        match group {
            Group::NonRecursive(binding) => {
                if outer.contains(&binding.id) {
                    return Err(ParseError::DuplicateDefinition(format!(
                        "local value {:?}",
                        binding.id
                    )));
                }
                self.check_heap_binding(binding, outer, depth)?;
                let mut scope = outer.clone();
                scope.insert(binding.id);
                Ok(scope)
            }
            Group::Recursive(bindings) => {
                if bindings.is_empty() {
                    return Err(ParseError::Malformed(
                        "recursive local group is empty".into(),
                    ));
                }
                self.check_table_len(bindings.len())?;
                let mut scope = outer.clone();
                for binding in bindings {
                    if !scope.insert(binding.id) {
                        return Err(ParseError::DuplicateDefinition(format!(
                            "local value {:?}",
                            binding.id
                        )));
                    }
                }
                for binding in bindings {
                    self.check_heap_binding(binding, &scope, depth)?;
                }
                Ok(scope)
            }
        }
    }

    fn check_join_group(
        &mut self,
        group: &Group<JoinBinding>,
        values: &BTreeSet<ValueId>,
        outer_joins: &BTreeMap<JoinId, SignatureId>,
        depth: usize,
    ) -> Result<BTreeMap<JoinId, SignatureId>, ParseError> {
        match group {
            Group::NonRecursive(binding) => {
                if outer_joins.contains_key(&binding.id) {
                    return Err(ParseError::DuplicateDefinition(format!(
                        "join {:?}",
                        binding.id
                    )));
                }
                self.check_join_binding(binding, values, outer_joins, depth)?;
                let mut body_joins = outer_joins.clone();
                body_joins.insert(binding.id, binding.signature);
                Ok(body_joins)
            }
            Group::Recursive(bindings) => {
                if bindings.is_empty() {
                    return Err(ParseError::Malformed(
                        "recursive join group is empty".into(),
                    ));
                }
                self.check_table_len(bindings.len())?;
                let mut joins = outer_joins.clone();
                for binding in bindings {
                    if joins.insert(binding.id, binding.signature).is_some() {
                        return Err(ParseError::DuplicateDefinition(format!(
                            "join {:?}",
                            binding.id
                        )));
                    }
                }
                for binding in bindings {
                    self.check_join_binding(binding, values, &joins, depth)?;
                }
                Ok(joins)
            }
        }
    }

    fn check_join_binding(
        &mut self,
        binding: &JoinBinding,
        values: &BTreeSet<ValueId>,
        joins: &BTreeMap<JoinId, SignatureId>,
        depth: usize,
    ) -> Result<(), ParseError> {
        self.check_signature(binding.signature)?;
        let signature = self.signature(binding.signature)?;
        if signature.arguments.len() != binding.parameters.len() {
            return Err(ParseError::InvalidSignature(
                "join parameter count does not match signature".into(),
            ));
        }
        self.check_unique_values(&binding.parameters, "join parameter")?;
        self.bump_node(depth)?;
        let mut scope = values.clone();
        scope.extend(binding.parameters.iter().copied());
        self.check_expr(&binding.body, &scope, joins, depth + 1)
    }

    fn check_alternatives(
        &mut self,
        alternatives: &[Alternative],
        values: &BTreeSet<ValueId>,
        joins: &BTreeMap<JoinId, SignatureId>,
        depth: usize,
    ) -> Result<(), ParseError> {
        if alternatives.is_empty() {
            return Err(ParseError::Malformed("case has no alternatives".into()));
        }
        self.check_table_len(alternatives.len())?;
        let mut patterns = BTreeSet::new();
        let mut has_default = false;
        for alternative in alternatives {
            self.bump_node(depth)?;
            let key = match &alternative.pattern {
                AlternativePattern::Default => {
                    if has_default {
                        return Err(ParseError::DuplicateDefinition(
                            "default alternative".into(),
                        ));
                    }
                    has_default = true;
                    "default".to_owned()
                }
                AlternativePattern::Constructor(id) => {
                    self.constructor(*id)?;
                    format!("constructor:{:?}", id)
                }
                AlternativePattern::Literal(literal) => {
                    self.check_scalar(literal)?;
                    format!("literal:{literal:?}")
                }
            };
            if !patterns.insert(key) {
                return Err(ParseError::DuplicateDefinition(
                    "case alternative pattern".into(),
                ));
            }
            let expected = match alternative.pattern {
                AlternativePattern::Constructor(id) => self.constructor(id)?.field_reps.len(),
                _ => 0,
            };
            if alternative.binders.len() != expected {
                return Err(ParseError::InvalidLayout(
                    "alternative binder count does not match constructor".into(),
                ));
            }
            self.check_unique_values(&alternative.binders, "alternative binder")?;
            let mut scope = values.clone();
            for binder in &alternative.binders {
                if !scope.insert(*binder) {
                    return Err(ParseError::DuplicateDefinition(format!(
                        "alternative binder {:?}",
                        binder
                    )));
                }
            }
            self.check_expr(&alternative.body, &scope, joins, depth + 1)?;
        }
        Ok(())
    }

    fn check_constructor_fields(
        &mut self,
        id: ConstructorId,
        fields: &[Atom],
        values: &BTreeSet<ValueId>,
    ) -> Result<(), ParseError> {
        let declaration = self.constructor(id)?;
        if declaration.field_reps.len() != fields.len() {
            return Err(ParseError::InvalidLayout(
                "constructor field count mismatch".into(),
            ));
        }
        self.check_atoms(fields, values)
    }

    fn check_captures(
        &mut self,
        captures: &[ValueRef],
        outer: &BTreeSet<ValueId>,
    ) -> Result<(), ParseError> {
        let mut unique = BTreeSet::new();
        for capture in captures {
            let key = match capture {
                ValueRef::Local(id) => {
                    if !outer.contains(id) {
                        return Err(ParseError::InvalidScope(format!(
                            "capture {:?} is out of scope",
                            id
                        )));
                    }
                    (0_u8, id.0)
                }
                ValueRef::Global(id) => {
                    self.global(*id)?;
                    (1_u8, id.0)
                }
            };
            if !unique.insert(key) {
                return Err(ParseError::DuplicateDefinition("capture".into()));
            }
        }
        Ok(())
    }

    fn closure_scope(&self, outer: &BTreeSet<ValueId>, captures: &[ValueRef]) -> BTreeSet<ValueId> {
        outer
            .iter()
            .copied()
            .filter(|id| self.top_values.contains(id))
            .chain(captures.iter().filter_map(|capture| match capture {
                ValueRef::Local(id) => Some(*id),
                ValueRef::Global(_) => None,
            }))
            .collect()
    }

    fn check_atoms(
        &mut self,
        atoms: &[Atom],
        values: &BTreeSet<ValueId>,
    ) -> Result<(), ParseError> {
        self.check_table_len(atoms.len())?;
        for atom in atoms {
            self.check_atom(atom, values)?;
        }
        Ok(())
    }

    fn check_atom(&mut self, atom: &Atom, values: &BTreeSet<ValueId>) -> Result<(), ParseError> {
        self.bump_work(1)?;
        match atom {
            Atom::Ref(ValueRef::Local(id)) if !values.contains(id) => Err(
                ParseError::InvalidScope(format!("value {:?} is out of scope", id)),
            ),
            Atom::Ref(ValueRef::Global(id)) => {
                self.global(*id)?;
                Ok(())
            }
            Atom::Scalar(literal) => self.check_scalar(literal),
            Atom::Ref(ValueRef::Local(_)) | Atom::Void => Ok(()),
        }
    }

    fn check_scalar(&mut self, literal: &ScalarLiteral) -> Result<(), ParseError> {
        match literal {
            ScalarLiteral::Int { bits, bytes } | ScalarLiteral::Word { bits, bytes } => {
                self.check_integer_width(*bits, bytes)
            }
            ScalarLiteral::Float { bits, bytes } => {
                if !matches!(*bits, 32 | 64) || bytes.len() != usize::from(*bits) / 8 {
                    return Err(ParseError::Malformed("invalid float literal width".into()));
                }
                Ok(())
            }
            ScalarLiteral::Char(value) if *value <= 0x10ffff => Ok(()),
            ScalarLiteral::Char(_) => Err(ParseError::Malformed(
                "character literal is outside Haskell codepoint range".into(),
            )),
            ScalarLiteral::Bytes(bytes) => {
                if bytes.len() > self.limits.max_string_bytes {
                    return Err(ParseError::LimitExceeded("string bytes"));
                }
                Ok(())
            }
        }
    }

    fn check_integer_width(&self, bits: u8, bytes: &[u8]) -> Result<(), ParseError> {
        if !matches!(bits, 8 | 16 | 32 | 64 | 128) || bytes.len() != usize::from(bits) / 8 {
            return Err(ParseError::Malformed(
                "invalid integer literal width".into(),
            ));
        }
        Ok(())
    }

    fn check_layout(
        &mut self,
        reps: &[RuntimeRep],
        layout: &CheckedLayout,
    ) -> Result<(), ParseError> {
        let stored: Vec<_> = reps
            .iter()
            .copied()
            .filter(|rep| *rep != RuntimeRep::Void)
            .collect();
        if layout.fields.len() != stored.len() || layout.root_mask.len() != stored.len() {
            return Err(ParseError::InvalidLayout(
                "stored fields/layout/root mask length mismatch".into(),
            ));
        }
        if layout.alignment == 0 || !layout.alignment.is_power_of_two() {
            return Err(ParseError::InvalidLayout("invalid layout alignment".into()));
        }
        let mut end = 0_u32;
        let mut natural_alignment = 1_u32;
        for (index, (field, rep)) in layout.fields.iter().zip(stored).enumerate() {
            if field.rep != rep {
                return Err(ParseError::InvalidLayout(
                    "layout field representation mismatch".into(),
                ));
            }
            let size = self.rep_size(rep)?;
            natural_alignment = natural_alignment.max(size.max(1));
            if field.offset < end
                || field.offset % size.max(1) != 0
                || field.offset.checked_add(size).is_none()
            {
                return Err(ParseError::InvalidLayout(
                    "overlapping or overflowing layout field".into(),
                ));
            }
            end = field.offset + size;
            let expected_root = matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef);
            if layout.root_mask[index] != expected_root {
                return Err(ParseError::InvalidLayout(
                    "incorrect layout root mask".into(),
                ));
            }
        }
        let expected_size = end
            .checked_add(natural_alignment - 1)
            .map(|value| value / natural_alignment * natural_alignment)
            .ok_or_else(|| ParseError::InvalidLayout("layout payload size overflows".into()))?;
        if layout.alignment != natural_alignment || layout.payload_size != expected_size {
            return Err(ParseError::InvalidLayout(
                "layout payload size is inconsistent".into(),
            ));
        }
        Ok(())
    }

    fn rep_size(&self, rep: RuntimeRep) -> Result<u32, ParseError> {
        match rep {
            RuntimeRep::Void => Ok(0),
            RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address => {
                Ok(u32::from(self.wire.envelope.target.pointer_width) / 8)
            }
            RuntimeRep::Int(bits) | RuntimeRep::Word(bits) | RuntimeRep::Float(bits) => {
                self.check_rep(rep)?;
                Ok(u32::from(bits) / 8)
            }
        }
    }

    fn check_rep(&self, rep: RuntimeRep) -> Result<(), ParseError> {
        let valid = match rep {
            RuntimeRep::Void
            | RuntimeRep::LiftedRef
            | RuntimeRep::UnliftedRef
            | RuntimeRep::Address => true,
            RuntimeRep::Int(bits) | RuntimeRep::Word(bits) => {
                matches!(bits, 8 | 16 | 32 | 64 | 128)
            }
            RuntimeRep::Float(bits) => matches!(bits, 32 | 64),
        };
        if valid {
            Ok(())
        } else {
            Err(ParseError::InvalidSignature(format!(
                "unsupported runtime representation {rep:?}"
            )))
        }
    }

    fn check_envelope(&mut self, requirements: &ProgramRequirements) -> Result<(), ParseError> {
        let envelope = &self.wire.envelope;
        if envelope.schema_version != SCHEMA_VERSION
            || requirements.schema_version != SCHEMA_VERSION
            || envelope.schema_version != requirements.schema_version
        {
            return Err(ParseError::UnsupportedVersion(envelope.schema_version));
        }
        if envelope.execution_abi_version != EXECUTION_ABI_VERSION
            || requirements.execution_abi_version != EXECUTION_ABI_VERSION
            || envelope.execution_abi_version != requirements.execution_abi_version
            || envelope.projection_profile != requirements.projection_profile
            || envelope.toolchain != requirements.toolchain
            || envelope.target != requirements.target
        {
            return Err(ParseError::UnsupportedTarget(format!(
                "artifact {:?} does not match requirements {:?}",
                envelope.target, requirements.target
            )));
        }
        self.check_text(&envelope.projection_profile)?;
        self.check_text(&envelope.toolchain)?;
        self.check_text(&envelope.target.abi)?;
        if !matches!(envelope.target.pointer_width, 32 | 64)
            || !matches!(envelope.target.word_width, 32 | 64)
        {
            return Err(ParseError::UnsupportedTarget(
                "unsupported pointer or word width".into(),
            ));
        }
        if !envelope
            .target
            .features
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        {
            return Err(ParseError::Malformed(
                "target features must be sorted and unique".into(),
            ));
        }
        for feature in &envelope.target.features {
            self.check_text(feature)?;
        }
        Ok(())
    }

    fn check_symbol(&mut self, symbol: &SymbolIdentity) -> Result<(), ParseError> {
        self.check_text(&symbol.unit)?;
        self.check_text(&symbol.module)?;
        self.check_text(&symbol.namespace)?;
        self.check_text(&symbol.occurrence)
    }

    fn check_text(&mut self, text: &str) -> Result<(), ParseError> {
        self.bump_work(text.len())?;
        if text.is_empty() {
            return Err(ParseError::Malformed("empty identity text".into()));
        }
        if text.len() > self.limits.max_string_bytes {
            return Err(ParseError::LimitExceeded("string bytes"));
        }
        Ok(())
    }

    fn check_unique_values(&self, ids: &[ValueId], kind: &str) -> Result<(), ParseError> {
        let mut unique = BTreeSet::new();
        for id in ids {
            if !unique.insert(*id) {
                return Err(ParseError::DuplicateDefinition(format!("{kind} {id:?}")));
            }
        }
        Ok(())
    }

    fn signature(&self, id: SignatureId) -> Result<&super::Signature, ParseError> {
        self.wire
            .signatures
            .get(id.0 as usize)
            .ok_or_else(|| ParseError::InvalidReference(format!("signature {:?}", id)))
    }

    fn check_signature(&self, id: SignatureId) -> Result<(), ParseError> {
        self.signature(id).map(|_| ())
    }

    fn global(&self, id: GlobalId) -> Result<&super::GlobalDecl, ParseError> {
        self.wire
            .globals
            .get(id.0 as usize)
            .ok_or_else(|| ParseError::InvalidReference(format!("global {:?}", id)))
    }

    fn constructor(&self, id: ConstructorId) -> Result<&super::ConstructorDecl, ParseError> {
        self.wire
            .constructors
            .get(id.0 as usize)
            .ok_or_else(|| ParseError::InvalidReference(format!("constructor {:?}", id)))
    }

    fn operation(&self, id: OperationId) -> Result<&super::OperationDecl, ParseError> {
        self.wire
            .operations
            .get(id.0 as usize)
            .ok_or_else(|| ParseError::InvalidReference(format!("operation {:?}", id)))
    }

    fn check_table_len(&self, len: usize) -> Result<(), ParseError> {
        if len > self.limits.max_table_entries {
            Err(ParseError::LimitExceeded("table entries"))
        } else {
            Ok(())
        }
    }

    fn bump_node(&mut self, depth: usize) -> Result<(), ParseError> {
        if depth > self.limits.max_depth {
            return Err(ParseError::LimitExceeded("expression depth"));
        }
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(ParseError::LimitExceeded("nodes"))?;
        if self.nodes > self.limits.max_nodes {
            return Err(ParseError::LimitExceeded("nodes"));
        }
        self.bump_work(1)
    }

    fn bump_work(&mut self, amount: usize) -> Result<(), ParseError> {
        self.work = self
            .work
            .checked_add(amount)
            .ok_or(ParseError::LimitExceeded("work"))?;
        if self.work > self.limits.max_work {
            return Err(ParseError::LimitExceeded("work"));
        }
        Ok(())
    }
}
