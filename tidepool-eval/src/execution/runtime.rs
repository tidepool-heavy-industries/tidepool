use std::collections::BTreeMap;

use tidepool_repr::execution_schema::{
    Alternative, AlternativePattern, Atom, Expr, GlobalDecl, GlobalId, Group, HeapBinding, HeapRhs,
    ImportedValue, JoinBinding, JoinId, LinkedProgram, TopBinding, UpdatePolicy, ValueId, ValueRef,
};

use super::{ReferenceError, ReferenceExecution, ReferenceValue};

#[derive(Clone)]
enum Value {
    Public(ReferenceValue),
    Function {
        parameters: Vec<ValueId>,
        bound: Vec<Value>,
        body: Expr,
        environment: Environment,
    },
}

type Environment = BTreeMap<ValueId, Value>;

#[derive(Clone)]
struct Join {
    parameters: Vec<ValueId>,
    body: Expr,
    environment: Environment,
}

enum Cache {
    Evaluating,
    Values(Vec<Value>),
    Consumed,
}

struct ProgramView<'a> {
    globals: &'a [GlobalDecl],
    imports: &'a [ImportedValue],
    bindings: &'a [Group<TopBinding>],
    entry: ValueId,
}

struct Machine<'program, 'inputs, 'execution> {
    globals: &'program [GlobalDecl],
    imports: &'program [ImportedValue],
    execution: &'execution mut ReferenceExecution<'inputs>,
    bindings: BTreeMap<ValueId, HeapRhs>,
    cache: BTreeMap<ValueId, Cache>,
    joins: BTreeMap<JoinId, Join>,
}

pub(super) fn execute(
    program: &LinkedProgram,
    execution: &mut ReferenceExecution<'_>,
) -> Result<Vec<ReferenceValue>, ReferenceError> {
    let prepared = program.prepared();
    execute_view(
        ProgramView {
            globals: prepared.globals(),
            imports: program.imports(),
            bindings: prepared.bindings(),
            entry: prepared.entry(),
        },
        execution,
    )
}

fn execute_view(
    view: ProgramView<'_>,
    execution: &mut ReferenceExecution<'_>,
) -> Result<Vec<ReferenceValue>, ReferenceError> {
    let mut machine = Machine {
        globals: view.globals,
        imports: view.imports,
        execution,
        bindings: BTreeMap::new(),
        cache: BTreeMap::new(),
        joins: BTreeMap::new(),
    };
    for group in view.bindings {
        match group {
            Group::NonRecursive(binding) => machine.install(&binding.binding),
            Group::Recursive(bindings) => {
                for binding in bindings {
                    machine.install(&binding.binding);
                }
            }
        }
    }
    let values = machine.force(view.entry, &Environment::new())?;
    values.into_iter().map(Value::publish).collect()
}

impl Value {
    fn publish(self) -> Result<ReferenceValue, ReferenceError> {
        match self {
            Self::Public(value) => Ok(value),
            Self::Function { .. } => Err(ReferenceError::FunctionResult),
        }
    }
}

impl Machine<'_, '_, '_> {
    fn step(&mut self) -> Result<(), ReferenceError> {
        self.execution.fuel = self
            .execution
            .fuel
            .checked_sub(1)
            .ok_or(ReferenceError::FuelExhausted)?;
        Ok(())
    }

    fn install(&mut self, binding: &HeapBinding) {
        self.bindings.insert(binding.id, binding.rhs.clone());
    }

    fn force(
        &mut self,
        id: ValueId,
        environment: &Environment,
    ) -> Result<Vec<Value>, ReferenceError> {
        self.step()?;
        if let Some(value) = environment.get(&id) {
            return Ok(vec![value.clone()]);
        }
        match self.cache.remove(&id) {
            Some(Cache::Values(values)) => {
                self.cache.insert(id, Cache::Values(values.clone()));
                return Ok(values);
            }
            Some(Cache::Evaluating) => {
                self.cache.insert(id, Cache::Evaluating);
                return Err(ReferenceError::Blackhole(id));
            }
            Some(Cache::Consumed) => {
                self.cache.insert(id, Cache::Consumed);
                return Err(ReferenceError::SingleEntryReentered);
            }
            None => {}
        }
        let rhs = self
            .bindings
            .get(&id)
            .cloned()
            .ok_or(ReferenceError::UnknownValue(id))?;
        self.cache.insert(id, Cache::Evaluating);
        let single_entry = matches!(
            rhs,
            HeapRhs::Thunk {
                update: UpdatePolicy::SingleEntry,
                ..
            }
        );
        match self.eval_rhs(rhs, environment) {
            Ok(values) => {
                self.cache.insert(
                    id,
                    if single_entry {
                        Cache::Consumed
                    } else {
                        Cache::Values(values.clone())
                    },
                );
                Ok(values)
            }
            Err(error) => {
                self.cache.insert(id, Cache::Consumed);
                Err(error)
            }
        }
    }

    fn eval_rhs(
        &mut self,
        rhs: HeapRhs,
        environment: &Environment,
    ) -> Result<Vec<Value>, ReferenceError> {
        match rhs {
            HeapRhs::Function {
                parameters, body, ..
            } => Ok(vec![Value::Function {
                parameters,
                bound: vec![],
                body: *body,
                environment: environment.clone(),
            }]),
            HeapRhs::Thunk { body, .. } => self.eval(&body, environment),
            HeapRhs::Constructor {
                constructor,
                fields,
            } => Ok(vec![Value::Public(ReferenceValue::Constructor {
                constructor,
                fields: fields
                    .iter()
                    .map(|atom| self.atom_public(atom, environment))
                    .collect::<Result<Vec<_>, _>>()?,
            })]),
        }
    }

    fn eval(
        &mut self,
        expression: &Expr,
        environment: &Environment,
    ) -> Result<Vec<Value>, ReferenceError> {
        self.step()?;
        match expression {
            Expr::Return(atoms) => atoms
                .iter()
                .map(|atom| self.atom(atom, environment))
                .collect(),
            Expr::Enter { callee, .. } => {
                let value = self.atom(callee, environment)?;
                if matches!(&value, Value::Function { parameters, bound, .. } if parameters.len() == bound.len())
                {
                    self.apply(value, vec![])
                } else {
                    Ok(vec![value])
                }
            }
            Expr::Call {
                callee, arguments, ..
            } => {
                let callee = self.atom(callee, environment)?;
                let arguments = arguments
                    .iter()
                    .map(|argument| self.atom(argument, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                self.apply(callee, arguments)
            }
            Expr::Operation {
                operation,
                arguments,
            } => {
                let arguments = arguments
                    .iter()
                    .map(|argument| self.atom_public(argument, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                self.execution
                    .operations
                    .call(*operation, &arguments)
                    .map(|values| values.into_iter().map(Value::Public).collect())
            }
            Expr::Construct {
                constructor,
                fields,
            } => Ok(vec![Value::Public(ReferenceValue::Constructor {
                constructor: *constructor,
                fields: fields
                    .iter()
                    .map(|field| self.atom_public(field, environment))
                    .collect::<Result<Vec<_>, _>>()?,
            })]),
            Expr::Case {
                scrutinee,
                binder,
                results,
                alternatives,
            } => {
                let value = one(self.eval(scrutinee, environment)?)?;
                let public = value.clone().publish()?;
                let alternative = select_alternative(alternatives, &public)
                    .ok_or(ReferenceError::NoAlternative)?;
                let mut inner = environment.clone();
                inner.insert(*binder, value);
                if let (
                    AlternativePattern::Constructor(_),
                    ReferenceValue::Constructor { fields, .. },
                ) = (&alternative.pattern, &public)
                {
                    if fields.len() != alternative.binders.len() {
                        return Err(ReferenceError::CaseBinders {
                            expected: fields.len(),
                            actual: alternative.binders.len(),
                        });
                    }
                    for (binder, field) in alternative.binders.iter().zip(fields) {
                        inner.insert(*binder, Value::Public(field.clone()));
                    }
                } else if !alternative.binders.is_empty() {
                    return Err(ReferenceError::CaseBinders {
                        expected: 0,
                        actual: alternative.binders.len(),
                    });
                }
                let values = self.eval(&alternative.body, &inner)?;
                if values.len() != results.len() {
                    return Err(ReferenceError::ResultArity {
                        expected: results.len(),
                        actual: values.len(),
                    });
                }
                Ok(values)
            }
            Expr::Let { bindings, body } => {
                self.install_group(bindings);
                self.eval(body, environment)
            }
            Expr::LetJoins { bindings, body } => {
                self.install_joins(bindings, environment);
                self.eval(body, environment)
            }
            Expr::Jump { join, arguments } => {
                let target = self
                    .joins
                    .get(join)
                    .cloned()
                    .ok_or(ReferenceError::UnknownJoin(*join))?;
                if target.parameters.len() != arguments.len() {
                    return Err(ReferenceError::Arity {
                        expected: target.parameters.len(),
                        actual: arguments.len(),
                    });
                }
                let mut inner = target.environment;
                for (parameter, argument) in target.parameters.iter().zip(arguments) {
                    inner.insert(*parameter, self.atom(argument, environment)?);
                }
                self.eval(&target.body, &inner)
            }
        }
    }

    fn install_group(&mut self, group: &Group<HeapBinding>) {
        match group {
            Group::NonRecursive(binding) => self.install(binding),
            Group::Recursive(bindings) => {
                for binding in bindings {
                    self.install(binding);
                }
            }
        }
    }

    fn install_joins(&mut self, group: &Group<JoinBinding>, environment: &Environment) {
        let bindings: Vec<&JoinBinding> = match group {
            Group::NonRecursive(binding) => vec![binding],
            Group::Recursive(bindings) => bindings.iter().collect(),
        };
        for binding in bindings {
            self.joins.insert(
                binding.id,
                Join {
                    parameters: binding.parameters.clone(),
                    body: (*binding.body).clone(),
                    environment: environment.clone(),
                },
            );
        }
    }

    fn atom(&mut self, atom: &Atom, environment: &Environment) -> Result<Value, ReferenceError> {
        match atom {
            Atom::Scalar(scalar) => Ok(Value::Public(ReferenceValue::Scalar(scalar.clone()))),
            Atom::Void => Ok(Value::Public(ReferenceValue::Void)),
            Atom::Ref(ValueRef::Local(id)) => one(self.force(*id, environment)?),
            Atom::Ref(ValueRef::Global(GlobalId(index))) => {
                let declaration = self
                    .globals
                    .get(*index as usize)
                    .ok_or(ReferenceError::UnknownGlobal(GlobalId(*index)))?;
                let linked = self
                    .imports
                    .get(*index as usize)
                    .ok_or(ReferenceError::InvalidLinkedImports)?;
                let imported = self
                    .execution
                    .imports
                    .values
                    .get(&declaration.identity)
                    .ok_or_else(|| ReferenceError::MissingImport(declaration.identity.clone()))?;
                if imported.generation != linked.generation {
                    return Err(ReferenceError::InvalidLinkedImports);
                }
                Ok(Value::Public(imported.value.clone()))
            }
        }
    }

    fn atom_public(
        &mut self,
        atom: &Atom,
        environment: &Environment,
    ) -> Result<ReferenceValue, ReferenceError> {
        self.atom(atom, environment)?.publish()
    }

    fn apply(
        &mut self,
        callee: Value,
        mut arguments: Vec<Value>,
    ) -> Result<Vec<Value>, ReferenceError> {
        let Value::Function {
            parameters,
            mut bound,
            body,
            mut environment,
        } = callee
        else {
            return Err(ReferenceError::NotCallable);
        };
        bound.append(&mut arguments);
        if bound.len() < parameters.len() {
            return Ok(vec![Value::Function {
                parameters,
                bound,
                body,
                environment,
            }]);
        }
        let extras = bound.split_off(parameters.len());
        for (parameter, value) in parameters.iter().zip(bound) {
            environment.insert(*parameter, value);
        }
        let values = self.eval(&body, &environment)?;
        if extras.is_empty() {
            Ok(values)
        } else {
            self.apply(one(values)?, extras)
        }
    }
}

fn one(mut values: Vec<Value>) -> Result<Value, ReferenceError> {
    if values.len() != 1 {
        return Err(ReferenceError::ResultArity {
            expected: 1,
            actual: values.len(),
        });
    }
    values.pop().ok_or(ReferenceError::ResultArity {
        expected: 1,
        actual: 0,
    })
}

fn select_alternative<'a>(
    alternatives: &'a [Alternative],
    value: &ReferenceValue,
) -> Option<&'a Alternative> {
    alternatives
        .iter()
        .find(|alternative| match (&alternative.pattern, value) {
            (
                AlternativePattern::Constructor(expected),
                ReferenceValue::Constructor { constructor, .. },
            ) => expected == constructor,
            (AlternativePattern::Literal(expected), ReferenceValue::Scalar(actual)) => {
                expected == actual
            }
            _ => false,
        })
        .or_else(|| {
            alternatives
                .iter()
                .find(|alternative| alternative.pattern == AlternativePattern::Default)
        })
}

#[cfg(test)]
mod tests {
    use tidepool_repr::execution_schema::{
        ConstructorId, GlobalDecl, JoinBinding, JoinId, OperationId, RuntimeRep, ScalarLiteral,
        Signature, SignatureId, SymbolIdentity, TopBinding,
    };

    use super::*;
    use crate::execution::{ReferenceImport, ReferenceImports, ReferenceOperations};

    fn symbol(name: &str) -> SymbolIdentity {
        SymbolIdentity {
            unit: "fixture".into(),
            module: "M3".into(),
            namespace: "value".into(),
            occurrence: name.into(),
        }
    }

    fn int(value: i64) -> ScalarLiteral {
        ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        }
    }

    #[derive(Default)]
    struct Add;

    impl ReferenceOperations for Add {
        fn call(
            &mut self,
            operation: OperationId,
            arguments: &[ReferenceValue],
        ) -> Result<Vec<ReferenceValue>, ReferenceError> {
            assert_eq!(operation, OperationId(0));
            let decode = |value: &ReferenceValue| match value {
                ReferenceValue::Scalar(ScalarLiteral::Int { bits: 64, bytes }) => {
                    let bytes: [u8; 8] = bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| ReferenceError::InvalidScalar("int64 width"))?;
                    Ok(i64::from_be_bytes(bytes))
                }
                _ => Err(ReferenceError::InvalidScalar("expected int64")),
            };
            Ok(vec![ReferenceValue::Scalar(int(
                decode(&arguments[0])? + decode(&arguments[1])?
            ))])
        }
    }

    fn run(
        globals: &[GlobalDecl],
        bindings: &[Group<TopBinding>],
        imports: &ReferenceImports,
    ) -> Result<Vec<ReferenceValue>, ReferenceError> {
        let linked = globals
            .iter()
            .map(|global| ImportedValue {
                identity: global.identity.clone(),
                signature: Signature {
                    arguments: vec![],
                    results: vec![RuntimeRep::LiftedRef],
                },
                evaluated: global.required_evaluated,
                generation: global.required_generation.unwrap_or(0),
            })
            .collect::<Vec<_>>();
        execute_view(
            ProgramView {
                globals,
                imports: &linked,
                bindings,
                entry: ValueId(0),
            },
            &mut ReferenceExecution {
                imports,
                operations: &mut Add,
                fuel: 1_000,
            },
        )
    }

    #[test]
    fn recursive_entry_reads_import() {
        let imported = symbol("imported");
        let globals = vec![GlobalDecl {
            identity: imported.clone(),
            signature: SignatureId(0),
            required_evaluated: true,
            required_generation: Some(7),
        }];
        let bindings = vec![Group::Recursive(vec![TopBinding {
            identity: symbol("entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Global(GlobalId(0))],
                    body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))])),
                },
            },
        }])];
        let imports = ReferenceImports {
            values: [(
                imported,
                ReferenceImport {
                    generation: 7,
                    value: ReferenceValue::Scalar(int(41)),
                },
            )]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            run(&globals, &bindings, &imports).unwrap(),
            vec![ReferenceValue::Scalar(int(41))]
        );
    }

    #[test]
    fn stale_materialized_import_generation_is_rejected() {
        let imported = symbol("imported");
        let globals = vec![GlobalDecl {
            identity: imported.clone(),
            signature: SignatureId(0),
            required_evaluated: true,
            required_generation: Some(7),
        }];
        let bindings = vec![Group::NonRecursive(TopBinding {
            identity: symbol("entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Global(GlobalId(0))],
                    body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))])),
                },
            },
        })];
        let imports = ReferenceImports {
            values: [(
                imported,
                ReferenceImport {
                    generation: 6,
                    value: ReferenceValue::Scalar(int(41)),
                },
            )]
            .into_iter()
            .collect(),
        };

        assert_eq!(
            run(&globals, &bindings, &imports),
            Err(ReferenceError::InvalidLinkedImports)
        );
    }

    #[test]
    fn partial_application_and_operation_execute() {
        let function = HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(2), ValueId(3)],
                captures: vec![],
                body: Box::new(Expr::Operation {
                    operation: OperationId(0),
                    arguments: vec![
                        Atom::Ref(ValueRef::Local(ValueId(2))),
                        Atom::Ref(ValueRef::Local(ValueId(3))),
                    ],
                }),
            },
        };
        let entry = TopBinding {
            identity: symbol("entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: Box::new(Expr::Let {
                        bindings: Group::NonRecursive(function),
                        body: Box::new(Expr::Let {
                            bindings: Group::NonRecursive(HeapBinding {
                                id: ValueId(4),
                                rhs: HeapRhs::Thunk {
                                    signature: SignatureId(0),
                                    update: UpdatePolicy::Memoize,
                                    captures: vec![],
                                    body: Box::new(Expr::Call {
                                        signature: SignatureId(0),
                                        callee: Atom::Ref(ValueRef::Local(ValueId(1))),
                                        arguments: vec![Atom::Scalar(int(20))],
                                    }),
                                },
                            }),
                            body: Box::new(Expr::Call {
                                signature: SignatureId(0),
                                callee: Atom::Ref(ValueRef::Local(ValueId(4))),
                                arguments: vec![Atom::Scalar(int(22))],
                            }),
                        }),
                    }),
                },
            },
        };
        assert_eq!(
            run(
                &[],
                &[Group::NonRecursive(entry)],
                &ReferenceImports::default()
            )
            .unwrap(),
            vec![ReferenceValue::Scalar(int(42))]
        );
    }

    #[test]
    fn single_entry_reentry_fails_before_operation() {
        let entry = TopBinding {
            identity: symbol("entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: Box::new(Expr::Let {
                        bindings: Group::NonRecursive(HeapBinding {
                            id: ValueId(1),
                            rhs: HeapRhs::Thunk {
                                signature: SignatureId(0),
                                update: UpdatePolicy::SingleEntry,
                                captures: vec![],
                                body: Box::new(Expr::Return(vec![Atom::Scalar(int(1))])),
                            },
                        }),
                        body: Box::new(Expr::Operation {
                            operation: OperationId(0),
                            arguments: vec![
                                Atom::Ref(ValueRef::Local(ValueId(1))),
                                Atom::Ref(ValueRef::Local(ValueId(1))),
                            ],
                        }),
                    }),
                },
            },
        };
        assert_eq!(
            run(
                &[],
                &[Group::NonRecursive(entry)],
                &ReferenceImports::default()
            ),
            Err(ReferenceError::SingleEntryReentered)
        );
    }

    #[test]
    fn constructor_case_join_and_multi_result_execute() {
        let entry = TopBinding {
            identity: symbol("entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: Box::new(Expr::Case {
                        scrutinee: Box::new(Expr::Construct {
                            constructor: ConstructorId(0),
                            fields: vec![Atom::Scalar(int(40))],
                        }),
                        binder: ValueId(1),
                        results: vec![RuntimeRep::Int(64), RuntimeRep::Int(64)],
                        alternatives: vec![Alternative {
                            pattern: AlternativePattern::Constructor(ConstructorId(0)),
                            binders: vec![ValueId(2)],
                            body: Expr::LetJoins {
                                bindings: Group::NonRecursive(JoinBinding {
                                    id: JoinId(0),
                                    signature: SignatureId(0),
                                    parameters: vec![ValueId(3), ValueId(4)],
                                    body: Box::new(Expr::Return(vec![
                                        Atom::Ref(ValueRef::Local(ValueId(3))),
                                        Atom::Ref(ValueRef::Local(ValueId(4))),
                                    ])),
                                }),
                                body: Box::new(Expr::Jump {
                                    join: JoinId(0),
                                    arguments: vec![
                                        Atom::Ref(ValueRef::Local(ValueId(2))),
                                        Atom::Scalar(int(2)),
                                    ],
                                }),
                            },
                        }],
                    }),
                },
            },
        };
        assert_eq!(
            run(
                &[],
                &[Group::NonRecursive(entry)],
                &ReferenceImports::default()
            )
            .unwrap(),
            vec![
                ReferenceValue::Scalar(int(40)),
                ReferenceValue::Scalar(int(2))
            ]
        );
    }

    #[test]
    fn blackhole_failure_does_not_mutate_imports() {
        let entry = TopBinding {
            identity: symbol("entry"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: Box::new(Expr::Return(vec![Atom::Ref(ValueRef::Local(ValueId(0)))])),
                },
            },
        };
        let imports = ReferenceImports::default();
        let before = imports.clone();
        assert_eq!(
            run(&[], &[Group::Recursive(vec![entry])], &imports),
            Err(ReferenceError::Blackhole(ValueId(0)))
        );
        assert_eq!(imports, before);
    }
}
