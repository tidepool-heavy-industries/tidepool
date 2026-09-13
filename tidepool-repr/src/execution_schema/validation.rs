use std::collections::{BTreeMap, BTreeSet};

use super::{
    Alternative, AlternativePattern, Atom, CaseKind, CheckedLayout, ConstructorId, DecodeLimits,
    Expr, ExprFrame, GlobalId, Group, HeapBinding, HeapRhs, JoinBinding, JoinId, OperationId,
    ParseError, ProgramRequirements, RuntimeRep, ScalarLiteral, SignatureId, SymbolIdentity,
    ValueId, ValueRef, WireProgram, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use recursion::{try_expand_and_collapse, MappableFrame, PartiallyApplied};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy)]
struct ValueType {
    rep: RuntimeRep,
    callable: Option<SignatureId>,
}

// The recursion driver schedules mapped children on a LIFO stack. Keeping
// children in reverse source order here makes its expansion order match the
// validator's established diagnostic order.
struct OrderedFrame<A> {
    children: Vec<A>,
    index: usize,
    mark: usize,
    expected: Option<Rc<Vec<RuntimeRep>>>,
}

impl MappableFrame for OrderedFrame<PartiallyApplied> {
    type Frame<A> = OrderedFrame<A>;

    fn map_frame<A, B>(input: OrderedFrame<A>, mut f: impl FnMut(A) -> B) -> OrderedFrame<B> {
        OrderedFrame {
            children: input.children.into_iter().map(&mut f).collect(),
            index: input.index,
            mark: input.mark,
            expected: input.expected,
        }
    }
}

fn child_indices(frame: &ExprFrame<usize>) -> Vec<usize> {
    let mut children = Vec::new();
    <ExprFrame<PartiallyApplied> as MappableFrame>::map_frame(frame.clone(), |child| {
        children.push(child);
        child
    });
    children
}

fn check_flat_tree(tree: &Expr, bindings: &[Group<super::TopBinding>]) -> Result<(), ParseError> {
    let len = tree.nodes.len();
    let mut parents = vec![0u8; len];
    for (index, frame) in tree.nodes.iter().enumerate() {
        for child in child_indices(frame) {
            if child >= index {
                return Err(ParseError::InvalidReference(format!(
                    "expression child {child} must precede parent {index}"
                )));
            }
            parents[child] = parents[child].saturating_add(1);
            if parents[child] > 1 {
                return Err(ParseError::InvalidReference(format!(
                    "expression child {child} has multiple parents"
                )));
            }
        }
    }
    let mut add_root = |binding: &super::TopBinding| -> Result<(), ParseError> {
        let root = match &binding.binding.rhs {
            HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } => *body,
            HeapRhs::Bytes(_) | HeapRhs::Constructor { .. } => return Ok(()),
        };
        let count = parents.get_mut(root).ok_or_else(|| {
            ParseError::InvalidReference(format!(
                "top-level expression root {root} is out of range"
            ))
        })?;
        *count = count.saturating_add(1);
        if *count > 1 {
            return Err(ParseError::InvalidReference(format!(
                "expression root {root} has multiple owners"
            )));
        }
        Ok(())
    };
    for group in bindings {
        match group {
            Group::NonRecursive(binding) => add_root(binding)?,
            Group::Recursive(bindings) => {
                for binding in bindings {
                    add_root(binding)?;
                }
            }
        }
    }
    if parents.contains(&0) {
        return Err(ParseError::InvalidReference(
            "expression arena contains an unreachable node".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ScopedValue {
    ty: ValueType,
    epoch: u64,
}

#[derive(Clone, Copy)]
struct ScopedJoin {
    signature: SignatureId,
    epoch: u64,
}

enum Undo {
    Value(usize, Option<ScopedValue>),
    Join(usize, Option<ScopedJoin>),
    Epoch(u64),
}

#[derive(Clone)]
enum Action {
    Value(ValueId, ValueType),
    Join(JoinId, SignatureId),
    Closure {
        captures: Vec<ValueRef>,
        parameters: Vec<(ValueId, ValueType)>,
    },
}

#[derive(Clone)]
struct Seed {
    index: usize,
    actions: Vec<Action>,
    expected: Option<Rc<Vec<RuntimeRep>>>,
}

struct Walker<'w, 'p> {
    validator: &'w mut Validator<'p>,
    tree: &'w Expr,
    values: Vec<Option<ScopedValue>>,
    joins: Vec<Option<ScopedJoin>>,
    undo: Vec<Undo>,
    epoch: u64,
    next_epoch: u64,
    typed: bool,
}

impl<'w, 'p> Walker<'w, 'p> {
    fn new(
        validator: &'w mut Validator<'p>,
        tree: &'w Expr,
        typed: bool,
    ) -> Result<Self, ParseError> {
        Ok(Self {
            validator,
            tree,
            values: Vec::new(),
            joins: Vec::new(),
            undo: Vec::new(),
            epoch: 1,
            next_epoch: 1,
            typed,
        })
    }

    fn value_index(&self, id: ValueId) -> Result<usize, ParseError> {
        let index = usize::try_from(id.0).map_err(|_| ParseError::LimitExceeded("value ids"))?;
        if index >= self.validator.limits.max_table_entries {
            Err(ParseError::LimitExceeded("value ids"))
        } else {
            Ok(index)
        }
    }

    fn join_index(&self, id: JoinId) -> Result<usize, ParseError> {
        let index = usize::try_from(id.0).map_err(|_| ParseError::LimitExceeded("join ids"))?;
        if index >= self.validator.limits.max_table_entries {
            Err(ParseError::LimitExceeded("join ids"))
        } else {
            Ok(index)
        }
    }

    fn ensure_value(&mut self, index: usize) {
        if index >= self.values.len() {
            self.values.resize(index + 1, None);
        }
    }

    fn ensure_join(&mut self, index: usize) {
        if index >= self.joins.len() {
            self.joins.resize(index + 1, None);
        }
    }

    fn value(&self, id: ValueId) -> Result<Option<ValueType>, ParseError> {
        let index = self.value_index(id)?;
        Ok(self
            .values
            .get(index)
            .and_then(|entry| *entry)
            .filter(|entry| entry.epoch == 0 || entry.epoch == self.epoch)
            .map(|entry| entry.ty))
    }

    fn join(&self, id: JoinId) -> Result<Option<SignatureId>, ParseError> {
        let index = self.join_index(id)?;
        Ok(self
            .joins
            .get(index)
            .and_then(|entry| *entry)
            .filter(|entry| entry.epoch == self.epoch)
            .map(|entry| entry.signature))
    }

    fn bind_value(&mut self, id: ValueId, ty: ValueType) -> Result<(), ParseError> {
        let index = self.value_index(id)?;
        self.ensure_value(index);
        let old = self.values[index].replace(ScopedValue {
            ty,
            epoch: self.epoch,
        });
        self.undo.push(Undo::Value(index, old));
        Ok(())
    }

    fn publish_top(&mut self, binding: &HeapBinding) -> Result<(), ParseError> {
        let index = self.value_index(binding.id)?;
        self.ensure_value(index);
        self.values[index] = Some(ScopedValue {
            ty: self.validator.binding_type(binding),
            epoch: 0,
        });
        Ok(())
    }

    fn bind_join(&mut self, id: JoinId, signature: SignatureId) -> Result<(), ParseError> {
        let index = self.join_index(id)?;
        self.ensure_join(index);
        let old = self.joins[index].replace(ScopedJoin {
            signature,
            epoch: self.epoch,
        });
        self.undo.push(Undo::Join(index, old));
        Ok(())
    }

    fn restore(&mut self, mark: usize) {
        while self.undo.len() > mark {
            match self.undo.pop().expect("undo mark") {
                Undo::Value(index, old) => self.values[index] = old,
                Undo::Join(index, old) => self.joins[index] = old,
                Undo::Epoch(epoch) => self.epoch = epoch,
            }
        }
    }

    fn apply(&mut self, actions: &[Action]) -> Result<(), ParseError> {
        for action in actions {
            match action {
                Action::Value(id, ty) => self.bind_value(*id, *ty)?,
                Action::Join(id, signature) => self.bind_join(*id, *signature)?,
                Action::Closure {
                    captures,
                    parameters,
                } => {
                    let mut local = Vec::new();
                    let mut unique = BTreeSet::new();
                    for capture in captures {
                        let key = match capture {
                            ValueRef::Local(id) => {
                                let ty = self.value(*id)?.ok_or_else(|| {
                                    ParseError::InvalidScope(format!(
                                        "capture {:?} is out of scope",
                                        id
                                    ))
                                })?;
                                local.push((*id, ty));
                                (0u8, id.0)
                            }
                            ValueRef::Global(id) => {
                                self.validator.global(*id)?;
                                (1u8, id.0)
                            }
                        };
                        if !unique.insert(key) {
                            return Err(ParseError::DuplicateDefinition("capture".into()));
                        }
                    }
                    self.undo.push(Undo::Epoch(self.epoch));
                    self.next_epoch = self
                        .next_epoch
                        .checked_add(1)
                        .ok_or(ParseError::LimitExceeded("scope epochs"))?;
                    self.epoch = self.next_epoch;
                    for (id, ty) in local.into_iter().chain(parameters.iter().copied()) {
                        self.bind_value(id, ty)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn register_value(&mut self, id: ValueId) -> Result<(), ParseError> {
        self.value_index(id)?;
        if !self.typed && !self.validator.defined_values.insert(id) {
            return Err(ParseError::DuplicateDefinition(format!(
                "value id {:?}",
                id
            )));
        }
        Ok(())
    }

    fn atom_type(&self, atom: &Atom) -> Result<ValueType, ParseError> {
        match atom {
            Atom::Ref(ValueRef::Local(id)) => self.value(*id)?.ok_or_else(|| {
                ParseError::InvalidScope(format!("value {:?} lacks representation evidence", id))
            }),
            Atom::Ref(ValueRef::Global(id)) => {
                let global = self.validator.global(*id)?;
                Ok(ValueType {
                    rep: global.rep,
                    callable: global.entry_signature,
                })
            }
            Atom::Scalar(literal) => Ok(ValueType {
                rep: literal.rep(),
                callable: None,
            }),
            Atom::Void => Ok(ValueType {
                rep: RuntimeRep::Void,
                callable: None,
            }),
            Atom::Rubbish(rep) => Ok(ValueType {
                rep: *rep,
                callable: None,
            }),
        }
    }

    fn check_atom(&mut self, atom: &Atom) -> Result<(), ParseError> {
        self.validator.bump_work(1)?;
        match atom {
            Atom::Ref(ValueRef::Local(id)) if self.value(*id)?.is_none() => Err(
                ParseError::InvalidScope(format!("value {:?} is out of scope", id)),
            ),
            Atom::Ref(ValueRef::Global(id)) => self.validator.global(*id).map(|_| ()),
            Atom::Scalar(literal) => self.validator.check_scalar(literal),
            Atom::Rubbish(RuntimeRep::Void) => Err(ParseError::Malformed(
                "rubbish must have one non-void representation after unarisation".into(),
            )),
            Atom::Rubbish(rep) => self.validator.check_rep(*rep),
            _ => Ok(()),
        }
    }

    fn check_atoms(&mut self, atoms: &[Atom]) -> Result<(), ParseError> {
        self.validator.check_table_len(atoms.len())?;
        for atom in atoms {
            self.check_atom(atom)?;
        }
        Ok(())
    }

    fn atom_reps(&self, atoms: &[Atom]) -> Result<Vec<RuntimeRep>, ParseError> {
        atoms
            .iter()
            .map(|atom| self.atom_type(atom).map(|ty| ty.rep))
            .collect()
    }

    fn check_atom_reps(
        &self,
        atoms: &[Atom],
        expected: &[RuntimeRep],
        context: &str,
    ) -> Result<(), ParseError> {
        let actual = self.atom_reps(atoms)?;
        if actual != expected {
            Err(ParseError::InvalidSignature(format!(
                "{context} {actual:?} do not match {expected:?}"
            )))
        } else {
            Ok(())
        }
    }

    fn check_callable(
        &self,
        atom: &Atom,
        declared: SignatureId,
        enter_only: bool,
    ) -> Result<super::Signature, ParseError> {
        let signature = self.validator.signature(declared)?.clone();
        let ty = self.atom_type(atom)?;
        if enter_only && !signature.arguments.is_empty() {
            return Err(ParseError::InvalidSignature(
                "entry demand cannot supply function arguments".into(),
            ));
        }
        if let Some(actual_id) = ty.callable {
            let actual = self.validator.signature(actual_id)?;
            let common = actual.arguments.len().min(signature.arguments.len());
            if actual.arguments[..common] != signature.arguments[..common] {
                return Err(ParseError::InvalidSignature(
                    "application prefix disagrees with entry signature".into(),
                ));
            }
            match signature.arguments.len().cmp(&actual.arguments.len()) {
                std::cmp::Ordering::Less if signature.results != [RuntimeRep::LiftedRef] => {
                    return Err(ParseError::InvalidSignature(
                        "partial application must return a function reference".into(),
                    ))
                }
                std::cmp::Ordering::Equal if signature.results != actual.results => {
                    return Err(ParseError::InvalidSignature(
                        "saturated application result disagrees with entry signature".into(),
                    ))
                }
                std::cmp::Ordering::Greater if actual.results != [RuntimeRep::LiftedRef] => {
                    return Err(ParseError::InvalidSignature(
                        "oversaturation requires a returned function reference".into(),
                    ))
                }
                _ => {}
            }
        } else if ty.rep != RuntimeRep::LiftedRef
            && (!signature.arguments.is_empty() || signature.results.as_slice() != [ty.rep])
        {
            return Err(ParseError::InvalidSignature(
                "callee is not a callable reference".into(),
            ));
        }
        Ok(signature)
    }

    fn rhs_seed<B>(
        &mut self,
        rhs: &HeapRhs<B>,
        body: usize,
        mut actions: Vec<Action>,
    ) -> Result<Option<Seed>, ParseError> {
        if !self.typed {
            self.validator.bump_node()?;
        }
        match rhs {
            HeapRhs::Bytes(bytes) => {
                self.validator.bump_work(bytes.len())?;
                Ok(None)
            }
            HeapRhs::Constructor {
                constructor,
                fields,
            } => {
                let reps = self.validator.constructor(*constructor)?.field_reps.clone();
                if reps.len() != fields.len() {
                    return Err(ParseError::InvalidLayout(
                        "constructor field count mismatch".into(),
                    ));
                }
                self.check_atoms(fields)?;
                if self.typed {
                    self.check_atom_reps(fields, &reps, "constructor fields")?;
                }
                Ok(None)
            }
            HeapRhs::Function {
                signature,
                parameters,
                captures,
                ..
            } => {
                let signature = self.validator.signature(*signature)?.clone();
                if signature.arguments.len() != parameters.len() {
                    return Err(ParseError::InvalidSignature(
                        "function parameter count does not match signature".into(),
                    ));
                }
                self.validator
                    .check_unique_values(parameters, "function parameter")?;
                for id in parameters {
                    self.register_value(*id)?;
                }
                let parameters = parameters
                    .iter()
                    .copied()
                    .zip(signature.arguments.iter().copied())
                    .map(|(id, rep)| {
                        (
                            id,
                            ValueType {
                                rep,
                                callable: None,
                            },
                        )
                    })
                    .collect();
                actions.push(Action::Closure {
                    captures: captures.clone(),
                    parameters,
                });
                Ok(Some(Seed {
                    index: body,
                    actions,
                    expected: Some(Rc::new(signature.results)),
                }))
            }
            HeapRhs::Thunk {
                signature,
                captures,
                ..
            } => {
                let signature = self.validator.signature(*signature)?.clone();
                if !signature.arguments.is_empty() {
                    return Err(ParseError::InvalidSignature(
                        "thunk signature has arguments".into(),
                    ));
                }
                actions.push(Action::Closure {
                    captures: captures.clone(),
                    parameters: Vec::new(),
                });
                Ok(Some(Seed {
                    index: body,
                    actions,
                    expected: Some(Rc::new(signature.results)),
                }))
            }
        }
    }

    fn walk(&mut self, seed: Option<Seed>) -> Result<(), ParseError> {
        let Some(seed) = seed else {
            return Ok(());
        };
        let state = RefCell::new(self);
        try_expand_and_collapse::<OrderedFrame<PartiallyApplied>, _, _, _>(
            seed,
            |seed| state.borrow_mut().expand(seed),
            |frame| state.borrow_mut().collapse(frame),
        )?;
        Ok(())
    }

    fn walk_top_binding(&mut self, binding: &HeapBinding) -> Result<(), ParseError> {
        let body = match &binding.rhs {
            HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } => *body,
            HeapRhs::Bytes(_) | HeapRhs::Constructor { .. } => 0,
        };
        let seed = self.rhs_seed(&binding.rhs, body, Vec::new())?;
        self.walk(seed)
    }

    fn expand(&mut self, seed: Seed) -> Result<OrderedFrame<Seed>, ParseError> {
        let mark = self.undo.len();
        self.apply(&seed.actions)?;
        let tree = self.tree;
        let frame = &tree.nodes[seed.index];
        if !self.typed {
            self.validator.bump_node()?;
        }
        let mut children = Vec::new();
        match frame {
            ExprFrame::Return(atoms) => self.check_atoms(atoms)?,
            ExprFrame::Enter { callee, signature } => {
                self.validator.check_signature(*signature)?;
                self.check_atom(callee)?;
            }
            ExprFrame::Call {
                callee,
                signature,
                arguments,
            } => {
                self.validator.check_signature(*signature)?;
                self.check_atom(callee)?;
                self.check_atoms(arguments)?;
            }
            ExprFrame::Operation {
                operation,
                arguments,
            } => {
                let signature_id = self.validator.operation(*operation)?.signature;
                let signature = self.validator.signature(signature_id)?;
                if signature.arguments.len() != arguments.len() {
                    return Err(ParseError::InvalidSignature(
                        "operation argument count does not match signature".into(),
                    ));
                }
                self.check_atoms(arguments)?;
            }
            ExprFrame::Construct {
                constructor,
                fields,
            } => {
                let declaration = self.validator.constructor(*constructor)?;
                if declaration.field_reps.len() != fields.len() {
                    return Err(ParseError::InvalidLayout(
                        "constructor field count mismatch".into(),
                    ));
                }
                self.check_atoms(fields)?;
            }
            ExprFrame::Jump { join, arguments } => {
                let signature_id = self.join(*join)?.ok_or_else(|| {
                    ParseError::InvalidScope(format!("join {:?} is out of scope", join))
                })?;
                let signature = self.validator.signature(signature_id)?;
                if signature.arguments.len() != arguments.len() {
                    return Err(ParseError::InvalidSignature(
                        "jump argument count does not match signature".into(),
                    ));
                }
                self.check_atoms(arguments)?;
            }
            ExprFrame::Case {
                scrutinee,
                binder,
                scrutinee_reps,
                kind,
                alternatives,
            } => {
                children.push(Seed {
                    index: *scrutinee,
                    actions: Vec::new(),
                    expected: Some(Rc::new(scrutinee_reps.clone())),
                });
                self.case_children(
                    *binder,
                    scrutinee_reps,
                    kind,
                    alternatives,
                    &seed.expected,
                    &mut children,
                )?;
            }
            ExprFrame::Let { bindings, body } => {
                let actions = self.local_children(bindings, &mut children)?;
                children.push(Seed {
                    index: *body,
                    actions,
                    expected: seed.expected.clone(),
                });
            }
            ExprFrame::LetJoins { bindings, body } => {
                let actions = self.join_children(bindings, &mut children)?;
                children.push(Seed {
                    index: *body,
                    actions,
                    expected: seed.expected.clone(),
                });
            }
        }
        children.reverse();
        Ok(OrderedFrame {
            children,
            index: seed.index,
            mark,
            expected: seed.expected,
        })
    }

    fn collapse(
        &mut self,
        mut frame: OrderedFrame<Vec<RuntimeRep>>,
    ) -> Result<Vec<RuntimeRep>, ParseError> {
        frame.children.reverse();
        let tree = self.tree;
        let actual = if self.typed {
            match &tree.nodes[frame.index] {
                ExprFrame::Return(atoms) => self.atom_reps(atoms)?,
                ExprFrame::Enter { callee, signature } => {
                    self.check_callable(callee, *signature, true)?.results
                }
                ExprFrame::Call {
                    callee,
                    signature,
                    arguments,
                } => {
                    let signature = self.check_callable(callee, *signature, false)?;
                    self.check_atom_reps(arguments, &signature.arguments, "call arguments")?;
                    signature.results
                }
                ExprFrame::Operation {
                    operation,
                    arguments,
                } => {
                    let signature = self
                        .validator
                        .signature(self.validator.operation(*operation)?.signature)?
                        .clone();
                    self.check_atom_reps(arguments, &signature.arguments, "operation arguments")?;
                    signature.results
                }
                ExprFrame::Construct {
                    constructor,
                    fields,
                } => {
                    let declaration = self.validator.constructor(*constructor)?;
                    self.check_atom_reps(fields, &declaration.field_reps, "constructor fields")?;
                    vec![declaration.result_rep]
                }
                ExprFrame::Jump { join, arguments } => {
                    let signature_id = self.join(*join)?.ok_or_else(|| {
                        ParseError::InvalidScope(format!("join {:?} is out of scope", join))
                    })?;
                    let signature = self.validator.signature(signature_id)?.clone();
                    self.check_atom_reps(arguments, &signature.arguments, "join arguments")?;
                    signature.results
                }
                ExprFrame::Case { .. } => {
                    let mut children = frame.children.into_iter();
                    children.next();
                    let first = children.next().unwrap_or_default();
                    for child in children {
                        if child != first {
                            return Err(ParseError::InvalidSignature(
                                "case alternatives return different representations".into(),
                            ));
                        }
                    }
                    first
                }
                ExprFrame::Let { .. } | ExprFrame::LetJoins { .. } => {
                    frame.children.pop().unwrap_or_default()
                }
            }
        } else {
            Vec::new()
        };
        if self.typed {
            if let Some(expected) = frame.expected {
                if actual != *expected {
                    return Err(ParseError::InvalidSignature(format!(
                        "expression representations {actual:?} do not match expected {expected:?}"
                    )));
                }
            }
        }
        self.restore(frame.mark);
        Ok(actual)
    }

    fn local_children(
        &mut self,
        bindings: &Group<HeapBinding>,
        children: &mut Vec<Seed>,
    ) -> Result<Vec<Action>, ParseError> {
        match bindings {
            Group::NonRecursive(binding) => {
                if self.value(binding.id)?.is_some() {
                    return Err(ParseError::DuplicateDefinition(format!(
                        "local value {:?}",
                        binding.id
                    )));
                }
                self.register_value(binding.id)?;
                let body = match &binding.rhs {
                    HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } => *body,
                    _ => 0,
                };
                if let Some(seed) = self.rhs_seed(&binding.rhs, body, Vec::new())? {
                    children.push(seed);
                }
                Ok(vec![Action::Value(
                    binding.id,
                    self.validator.binding_type(binding),
                )])
            }
            Group::Recursive(bindings) => {
                if bindings.is_empty() {
                    return Err(ParseError::Malformed(
                        "recursive local group is empty".into(),
                    ));
                }
                self.validator.check_table_len(bindings.len())?;
                let mut seen = BTreeSet::new();
                let mut actions = Vec::new();
                for binding in bindings {
                    if self.value(binding.id)?.is_some() || !seen.insert(binding.id) {
                        return Err(ParseError::DuplicateDefinition(format!(
                            "local value {:?}",
                            binding.id
                        )));
                    }
                    self.register_value(binding.id)?;
                    actions.push(Action::Value(
                        binding.id,
                        self.validator.binding_type(binding),
                    ));
                }
                let mark = self.undo.len();
                self.apply(&actions)?;
                for binding in bindings {
                    let body = match &binding.rhs {
                        HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } => *body,
                        _ => 0,
                    };
                    if let Some(seed) = self.rhs_seed(&binding.rhs, body, actions.clone())? {
                        children.push(seed);
                    }
                }
                self.restore(mark);
                Ok(actions)
            }
        }
    }

    fn join_children(
        &mut self,
        bindings: &Group<JoinBinding>,
        children: &mut Vec<Seed>,
    ) -> Result<Vec<Action>, ParseError> {
        let all: Vec<_> = match bindings {
            Group::NonRecursive(binding) => vec![binding],
            Group::Recursive(bindings) => {
                if bindings.is_empty() {
                    return Err(ParseError::Malformed(
                        "recursive join group is empty".into(),
                    ));
                }
                self.validator.check_table_len(bindings.len())?;
                bindings.iter().collect()
            }
        };
        let mut seen = BTreeSet::new();
        let mut actions = Vec::new();
        for binding in &all {
            self.join_index(binding.id)?;
            if self.join(binding.id)?.is_some() || !seen.insert(binding.id) {
                return Err(ParseError::DuplicateDefinition(format!(
                    "join {:?}",
                    binding.id
                )));
            }
            actions.push(Action::Join(binding.id, binding.signature));
        }
        for binding in all {
            let signature = self.validator.signature(binding.signature)?.clone();
            if signature.arguments.len() != binding.parameters.len() {
                return Err(ParseError::InvalidSignature(
                    "join parameter count does not match signature".into(),
                ));
            }
            self.validator
                .check_unique_values(&binding.parameters, "join parameter")?;
            if !self.typed {
                self.validator.bump_node()?;
            }
            for id in &binding.parameters {
                self.register_value(*id)?;
            }
            let mut child_actions = if matches!(bindings, Group::Recursive(_)) {
                actions.clone()
            } else {
                Vec::new()
            };
            child_actions.extend(
                binding
                    .parameters
                    .iter()
                    .copied()
                    .zip(signature.arguments.iter().copied())
                    .map(|(id, rep)| {
                        Action::Value(
                            id,
                            ValueType {
                                rep,
                                callable: None,
                            },
                        )
                    }),
            );
            children.push(Seed {
                index: binding.body,
                actions: child_actions,
                expected: Some(Rc::new(signature.results)),
            });
        }
        Ok(actions)
    }

    fn case_children(
        &mut self,
        binder: ValueId,
        scrutinee_reps: &[RuntimeRep],
        kind: &CaseKind,
        alternatives: &[Alternative],
        expected: &Option<Rc<Vec<RuntimeRep>>>,
        children: &mut Vec<Seed>,
    ) -> Result<(), ParseError> {
        for rep in scrutinee_reps {
            self.validator.check_rep(*rep)?;
        }
        if alternatives.is_empty() {
            return Err(ParseError::Malformed("case has no alternatives".into()));
        }
        match kind {
            CaseKind::Algebraic(family) => {
                self.validator.check_symbol(family)?;
                if !matches!(
                    scrutinee_reps,
                    [RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef]
                ) {
                    return Err(ParseError::InvalidSignature(
                        "algebraic case requires a managed reference".into(),
                    ));
                }
            }
            CaseKind::Primitive(rep) => {
                self.validator.check_rep(*rep)?;
                if matches!(
                    rep,
                    RuntimeRep::Void | RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef
                ) || scrutinee_reps != [*rep]
                {
                    return Err(ParseError::InvalidSignature(
                        "primitive case scrutinee representation mismatch".into(),
                    ));
                }
            }
            CaseKind::MultiValue | CaseKind::Polymorphic => {
                if alternatives.len() != 1 || alternatives[0].pattern != AlternativePattern::Default
                {
                    return Err(ParseError::Malformed(
                        "multi-value and polymorphic cases require one DEFAULT alternative".into(),
                    ));
                }
                if *kind == CaseKind::MultiValue && scrutinee_reps.contains(&RuntimeRep::Void) {
                    return Err(ParseError::InvalidSignature(
                        "multi-value case contains a nonphysical Void component".into(),
                    ));
                }
            }
        }
        let mut case_actions = Vec::new();
        if *kind == CaseKind::MultiValue {
            self.register_value(binder)?;
        }
        if *kind != CaseKind::MultiValue {
            if self.value(binder)?.is_some() {
                return Err(ParseError::DuplicateDefinition(format!(
                    "case binder {:?}",
                    binder
                )));
            }
            self.register_value(binder)?;
            if let [rep] = scrutinee_reps {
                case_actions.push(Action::Value(
                    binder,
                    ValueType {
                        rep: *rep,
                        callable: None,
                    },
                ));
            } else if !self.typed {
                case_actions.push(Action::Value(
                    binder,
                    ValueType {
                        rep: RuntimeRep::Void,
                        callable: None,
                    },
                ));
            }
        }
        self.validator.check_table_len(alternatives.len())?;
        let mut patterns = BTreeSet::new();
        for alternative in alternatives {
            if !self.typed {
                self.validator.bump_node()?;
            }
            let key = match &alternative.pattern {
                AlternativePattern::Default => PatternKey::Default,
                AlternativePattern::Constructor(id) => {
                    let constructor = self.validator.constructor(*id)?;
                    if !matches!(kind, CaseKind::Algebraic(family) if *family == constructor.family)
                    {
                        return Err(ParseError::InvalidLayout(
                            "constructor alternative disagrees with case family or kind".into(),
                        ));
                    }
                    PatternKey::Constructor(*id)
                }
                AlternativePattern::Literal(literal) => {
                    self.validator.check_scalar(literal)?;
                    if !matches!(kind, CaseKind::Primitive(rep) if *rep == literal.rep()) {
                        return Err(ParseError::InvalidSignature(
                            "literal alternative disagrees with case representation or kind".into(),
                        ));
                    }
                    let bytes = match literal {
                        ScalarLiteral::Int { bytes, .. }
                        | ScalarLiteral::Word { bytes, .. }
                        | ScalarLiteral::Float { bytes, .. }
                        | ScalarLiteral::Bytes(bytes) => bytes.clone(),
                        ScalarLiteral::Char(value) => value.to_be_bytes().to_vec(),
                        ScalarLiteral::NullAddress => vec![],
                    };
                    if matches!(literal, ScalarLiteral::NullAddress) {
                        PatternKey::NullAddress
                    } else {
                        PatternKey::Literal(literal.rep(), bytes)
                    }
                }
            };
            if !patterns.insert(key) {
                return Err(ParseError::DuplicateDefinition(
                    "case alternative pattern".into(),
                ));
            }
            let reps: Vec<_> = match (&alternative.pattern, kind) {
                (_, CaseKind::MultiValue) => scrutinee_reps.to_vec(),
                (AlternativePattern::Constructor(id), _) => {
                    self.validator.constructor(*id)?.field_reps.clone()
                }
                _ => Vec::new(),
            };
            if alternative.binders.len() != reps.len() {
                return Err(ParseError::InvalidLayout(
                    "alternative binder count does not match constructor".into(),
                ));
            }
            self.validator
                .check_unique_values(&alternative.binders, "alternative binder")?;
            let mut actions = case_actions.clone();
            for (id, rep) in alternative.binders.iter().copied().zip(reps) {
                if self.value(id)?.is_some()
                    || (case_actions
                        .iter()
                        .any(|action| matches!(action, Action::Value(other, _) if *other == id)))
                {
                    return Err(ParseError::DuplicateDefinition(format!(
                        "alternative binder {:?}",
                        id
                    )));
                }
                self.register_value(id)?;
                actions.push(Action::Value(
                    id,
                    ValueType {
                        rep,
                        callable: None,
                    },
                ));
            }
            children.push(Seed {
                index: alternative.body,
                actions,
                expected: expected.clone(),
            });
        }
        Ok(())
    }
}
#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum PatternKey {
    Default,
    Constructor(ConstructorId),
    NullAddress,
    Literal(RuntimeRep, Vec<u8>),
}

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
            expressions: Expr {
                nodes: vec![ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 42_i64.to_be_bytes().to_vec(),
                })])],
            },
            bindings: vec![Group::NonRecursive(TopBinding {
                identity: symbol("entry"),
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: 0,
                    },
                },
            })],
            entry: ValueId(0),
        }
    }

    fn replace_root(program: &mut WireProgram, frame: ExprFrame<usize>) {
        program.expressions.nodes = vec![frame];
    }

    fn set_top_rhs(program: &mut WireProgram, rhs: HeapRhs) {
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        binding.binding.rhs = rhs;
    }

    fn empty_constructor(name: &str, tag: u32, family_size: u32) -> ConstructorDecl {
        ConstructorDecl {
            identity: symbol(name),
            family: symbol("Family"),
            tag,
            family_size,
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        }
    }

    #[test]
    fn constructor_tags_preserve_partial_family_evidence() {
        let mut program = valid_program();
        program.constructors = vec![empty_constructor("OnlyEncountered", 3, 5)];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        for (tag, family_size) in [(0, 5), (1, 0), (6, 5)] {
            program.constructors[0].tag = tag;
            program.constructors[0].family_size = family_size;
            assert!(validate_program(&program, &requirements(), DecodeLimits::default()).is_err());
        }
    }

    #[test]
    fn constructor_family_tags_must_be_unique_and_sizes_agree() {
        let mut program = valid_program();
        program.constructors = vec![empty_constructor("A", 1, 3), empty_constructor("B", 2, 3)];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        program.constructors[1].tag = 1;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition(_))
        ));
        program.constructors[1].tag = 2;
        program.constructors[1].family_size = 4;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
    }

    #[test]
    fn unsupported_scalar_widths_never_reach_native_layout() {
        for rep in [
            RuntimeRep::Int(128),
            RuntimeRep::Word(128),
            RuntimeRep::Int(24),
        ] {
            let mut program = valid_program();
            program.signatures[0].results = vec![rep];
            assert!(matches!(
                validate_program(&program, &requirements(), DecodeLimits::default()),
                Err(ParseError::InvalidSignature(_))
            ));
            assert!(crate::execution_schema::StorageLayout::for_reps(&target(), &[rep]).is_err());
        }
    }

    #[test]
    fn accepts_representative_valid_program() {
        validate_program(&valid_program(), &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn rubbish_preserves_representation_without_becoming_a_scalar() {
        for rep in [
            RuntimeRep::LiftedRef,
            RuntimeRep::UnliftedRef,
            RuntimeRep::Address,
            RuntimeRep::Int(64),
            RuntimeRep::Float(64),
        ] {
            let mut program = valid_program();
            program.signatures[0].results = vec![rep];
            replace_root(&mut program, ExprFrame::Return(vec![Atom::Rubbish(rep)]));
            validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        }
    }

    #[test]
    fn rubbish_requires_one_valid_nonvoid_representation() {
        for rep in [RuntimeRep::Void, RuntimeRep::Int(7)] {
            let mut program = valid_program();
            replace_root(&mut program, ExprFrame::Return(vec![Atom::Rubbish(rep)]));
            assert!(validate_program(&program, &requirements(), DecodeLimits::default()).is_err());
        }
    }

    #[test]
    fn null_address_is_not_a_managed_reference() {
        let mut program = valid_program();
        replace_root(
            &mut program,
            ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::NullAddress)]),
        );
        program.signatures[0].results = vec![RuntimeRep::Address];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        program.signatures[0].results = vec![RuntimeRep::LiftedRef];
        assert!(validate_program(&program, &requirements(), DecodeLimits::default()).is_err());
    }

    #[test]
    fn imported_atomic_values_keep_their_declared_representation() {
        for rep in [
            RuntimeRep::Address,
            RuntimeRep::UnliftedRef,
            RuntimeRep::Word(64),
        ] {
            let mut program = valid_program();
            program.globals.push(crate::execution_schema::GlobalDecl {
                identity: symbol("imported_value"),
                rep,
                entry_signature: None,
                required_evaluated: true,
                required_generation: None,
            });
            program.signatures[0].results = vec![rep];
            replace_root(
                &mut program,
                ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]),
            );
            validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
            program.signatures[0].results = vec![RuntimeRep::LiftedRef];
            assert!(validate_program(&program, &requirements(), DecodeLimits::default()).is_err());
        }
    }

    #[test]
    fn raw_global_cannot_claim_a_callable_entry() {
        let mut program = valid_program();
        program.globals.push(crate::execution_schema::GlobalDecl {
            identity: symbol("address"),
            rep: RuntimeRep::Address,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: true,
            required_generation: None,
        });
        assert!(validate_program(&program, &requirements(), DecodeLimits::default()).is_err());
    }

    fn integer(value: i64) -> Atom {
        Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    }

    fn with_case(
        kind: CaseKind,
        reps: Vec<RuntimeRep>,
        alternatives: Vec<Alternative>,
    ) -> WireProgram {
        let mut program = valid_program();
        let mut nodes = vec![ExprFrame::Return(vec![integer(1)])];
        let alternatives: Vec<_> = alternatives
            .into_iter()
            .map(|mut alternative| {
                alternative.body = nodes.len();
                nodes.push(ExprFrame::Return(vec![integer(42)]));
                alternative
            })
            .collect();
        let root = nodes.len();
        nodes.push(ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(1),
            scrutinee_reps: reps,
            kind,
            alternatives,
        });
        program.expressions.nodes = nodes;
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = root;
        program
    }

    fn default_alternative() -> Alternative {
        Alternative {
            pattern: AlternativePattern::Default,
            binders: vec![],
            body: 0,
        }
    }

    #[test]
    fn case_literals_must_match_authoritative_representation() {
        let mut alternative = default_alternative();
        alternative.pattern = AlternativePattern::Literal(ScalarLiteral::Word {
            bits: 32,
            bytes: 1_u32.to_be_bytes().to_vec(),
        });
        let program = with_case(
            CaseKind::Primitive(RuntimeRep::Int(64)),
            vec![RuntimeRep::Int(64)],
            vec![alternative],
        );
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidSignature(_))
        ));
    }

    #[test]
    fn refined_primitive_case_does_not_claim_exhaustiveness() {
        let mut alternative = default_alternative();
        alternative.pattern = AlternativePattern::Literal(ScalarLiteral::Int {
            bits: 64,
            bytes: 1_i64.to_be_bytes().to_vec(),
        });
        let program = with_case(
            CaseKind::Primitive(RuntimeRep::Int(64)),
            vec![RuntimeRep::Int(64)],
            vec![alternative],
        );
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn multivalue_case_binds_components_and_not_the_dead_case_binder() {
        let mut alternative = default_alternative();
        alternative.binders = vec![ValueId(2)];
        let mut program = with_case(
            CaseKind::MultiValue,
            vec![RuntimeRep::Int(64)],
            vec![alternative],
        );
        program.expressions.nodes[1] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(2)))]);
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        program.expressions.nodes[1] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(_))
        ));
    }

    #[test]
    fn polymorphic_case_requires_one_default() {
        let valid = with_case(
            CaseKind::Polymorphic,
            vec![RuntimeRep::Int(64)],
            vec![default_alternative()],
        );
        validate_program(&valid, &requirements(), DecodeLimits::default()).unwrap();
        let invalid = with_case(
            CaseKind::Polymorphic,
            vec![RuntimeRep::Int(64)],
            vec![default_alternative(), default_alternative()],
        );
        assert!(validate_program(&invalid, &requirements(), DecodeLimits::default()).is_err());
    }

    #[test]
    fn algebraic_cases_reject_mixed_families_and_literal_patterns() {
        let mut program = with_case(
            CaseKind::Algebraic(symbol("T")),
            vec![RuntimeRep::LiftedRef],
            vec![Alternative {
                pattern: AlternativePattern::Constructor(ConstructorId(0)),
                binders: vec![],
                body: 0,
            }],
        );
        program.constructors.push(ConstructorDecl {
            identity: symbol("C"),
            family: symbol("Other"),
            tag: 1,
            family_size: 1,
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        program.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
        program.constructors[0].family = symbol("T");
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        let mut case = program.expressions.nodes.pop().unwrap();
        program
            .expressions
            .nodes
            .push(ExprFrame::Return(vec![integer(42)]));
        let ExprFrame::Case { alternatives, .. } = &mut case else {
            unreachable!()
        };
        alternatives.push(Alternative {
            pattern: AlternativePattern::Literal(ScalarLiteral::Int {
                bits: 64,
                bytes: 0_i64.to_be_bytes().to_vec(),
            }),
            binders: vec![],
            body: 2,
        });
        program.expressions.nodes.push(case);
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = 3;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidSignature(_))
        ));
    }

    #[test]
    fn rejects_out_of_scope_value_without_publishing() {
        let mut program = valid_program();
        replace_root(
            &mut program,
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(99)))]),
        );
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
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]),
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(2)))]),
            ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(1),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: 0,
                    },
                }),
                body: 1,
            },
        ];
        set_top_rhs(
            &mut program,
            HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(1)],
                captures: vec![],
                body: 2,
            },
        );
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
        set_top_rhs(
            &mut program,
            HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![],
                captures: vec![],
                body: 0,
            },
        );
        assert_invalid_signature(program);
    }

    #[test]
    fn rejects_call_argument_with_wrong_representation() {
        let mut program = valid_program();
        program.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        program.signatures.push(Signature {
            arguments: vec![],
            results: vec![RuntimeRep::Int(64)],
        });
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]),
            ExprFrame::Call {
                signature: SignatureId(0),
                callee: Atom::Ref(ValueRef::Local(ValueId(0))),
                arguments: vec![Atom::Scalar(ScalarLiteral::Word {
                    bits: 64,
                    bytes: 1_u64.to_be_bytes().to_vec(),
                })],
            },
        ];
        set_top_rhs(
            &mut program,
            HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(1)],
                captures: vec![],
                body: 0,
            },
        );
        program.bindings.push(Group::NonRecursive(TopBinding {
            identity: symbol("caller"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(1),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: 1,
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
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]),
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(2)))]),
            ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: vec![],
                        captures: vec![ValueRef::Local(ValueId(1))],
                        body: 0,
                    },
                }),
                body: 1,
            },
        ];
        set_top_rhs(
            &mut program,
            HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(1)],
                captures: vec![],
                body: 2,
            },
        );
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
        operation.signatures.push(Signature {
            arguments: vec![],
            results: vec![RuntimeRep::Int(64)],
        });
        let Group::NonRecursive(binding) = &mut operation.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { signature, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *signature = SignatureId(1);
        replace_root(
            &mut operation,
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![wrong.clone()],
            },
        );
        assert_invalid_signature(operation);

        let mut jump = valid_program();
        jump.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        jump.signatures.push(Signature {
            arguments: vec![],
            results: vec![RuntimeRep::Int(64)],
        });
        jump.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]),
            ExprFrame::Jump {
                join: JoinId(0),
                arguments: vec![wrong],
            },
            ExprFrame::LetJoins {
                bindings: Group::NonRecursive(JoinBinding {
                    id: JoinId(0),
                    signature: SignatureId(0),
                    parameters: vec![ValueId(1)],
                    body: 0,
                }),
                body: 1,
            },
        ];
        let Group::NonRecursive(binding) = &mut jump.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk {
            signature, body, ..
        } = &mut binding.binding.rhs
        else {
            unreachable!()
        };
        *signature = SignatureId(1);
        *body = 2;
        assert_invalid_signature(jump);
    }

    #[test]
    fn rejects_unnaturally_aligned_managed_layout() {
        let mut program = valid_program();
        program.constructors.push(ConstructorDecl {
            identity: symbol("C"),
            family: symbol("T"),
            tag: 1,
            family_size: 1,
            result_rep: RuntimeRep::LiftedRef,
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

    #[test]
    fn deep_case_arena_validates_and_drops_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut program = valid_program();
                let mut nodes = vec![ExprFrame::Return(vec![integer(42)])];
                let mut root = 0;
                for id in 1..=20_000 {
                    let alternative = nodes.len();
                    nodes.push(ExprFrame::Return(vec![integer(42)]));
                    let next = nodes.len();
                    nodes.push(ExprFrame::Case {
                        scrutinee: root,
                        binder: ValueId(id),
                        scrutinee_reps: vec![RuntimeRep::Int(64)],
                        kind: CaseKind::Primitive(RuntimeRep::Int(64)),
                        alternatives: vec![Alternative {
                            pattern: AlternativePattern::Default,
                            binders: vec![],
                            body: alternative,
                        }],
                    });
                    root = next;
                }
                program.expressions.nodes = nodes;
                let Group::NonRecursive(binding) = &mut program.bindings[0] else {
                    unreachable!()
                };
                let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
                    unreachable!()
                };
                *body = root;
                validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn flat_forest_rejects_forward_shared_and_unowned_nodes() {
        let mut program = valid_program();
        program.expressions.nodes = vec![
            ExprFrame::Case {
                scrutinee: 1,
                binder: ValueId(1),
                scrutinee_reps: vec![RuntimeRep::Int(64)],
                kind: CaseKind::Primitive(RuntimeRep::Int(64)),
                alternatives: vec![default_alternative()],
            },
            ExprFrame::Return(vec![integer(42)]),
        ];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(_))
        ));

        program.expressions.nodes = vec![
            ExprFrame::Return(vec![integer(42)]),
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(1),
                scrutinee_reps: vec![RuntimeRep::Int(64)],
                kind: CaseKind::Primitive(RuntimeRep::Int(64)),
                alternatives: vec![Alternative {
                    body: 0,
                    ..default_alternative()
                }],
            },
        ];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = 1;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(_))
        ));

        program.expressions.nodes = vec![
            ExprFrame::Return(vec![integer(1)]),
            ExprFrame::Return(vec![integer(42)]),
        ];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(_))
        ));
    }

    #[test]
    fn rejects_sparse_value_id_before_dense_scope_allocation() {
        let mut program = valid_program();
        replace_root(
            &mut program,
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(u32::MAX)))]),
        );
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::LimitExceeded("value ids"))
        ));
    }

    #[test]
    fn recursive_local_capture_sees_siblings_but_nonrecursive_capture_does_not() {
        let mut program = valid_program();
        program.signatures.push(Signature {
            arguments: vec![],
            results: vec![RuntimeRep::LiftedRef],
        });
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]),
            ExprFrame::Return(vec![integer(42)]),
            ExprFrame::Let {
                bindings: Group::Recursive(vec![HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(1),
                        update: UpdatePolicy::Memoize,
                        captures: vec![ValueRef::Local(ValueId(1))],
                        body: 0,
                    },
                }]),
                body: 1,
            },
        ];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = 2;
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        let ExprFrame::Let { bindings, .. } = &mut program.expressions.nodes[2] else {
            unreachable!()
        };
        let binding = match std::mem::replace(bindings, Group::Recursive(Vec::new())) {
            Group::Recursive(mut bindings) => bindings.pop().unwrap(),
            Group::NonRecursive(_) => unreachable!(),
        };
        *bindings = Group::NonRecursive(binding);
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(_))
        ));
    }

    #[test]
    fn value_binder_ids_are_unique_across_top_closures() {
        let mut program = valid_program();
        program.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]),
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]),
        ];
        set_top_rhs(
            &mut program,
            HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![ValueId(1)],
                captures: vec![],
                body: 0,
            },
        );
        program.bindings.push(Group::NonRecursive(TopBinding {
            identity: symbol("other"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Function {
                    signature: SignatureId(0),
                    parameters: vec![ValueId(1)],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition(_))
        ));
    }

    #[test]
    fn semantic_children_report_errors_in_source_order() {
        let mut program = with_case(
            CaseKind::Primitive(RuntimeRep::Int(64)),
            vec![RuntimeRep::Int(64)],
            vec![default_alternative()],
        );
        program.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(99)))]);
        program.expressions.nodes[1] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(98)))]);
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(message)) if message.contains("ValueId(99)")
        ));

        let mut program = valid_program();
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(99)))]),
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(98)))]),
            ExprFrame::Return(vec![integer(42)]),
            ExprFrame::Let {
                bindings: Group::Recursive(vec![
                    HeapBinding {
                        id: ValueId(1),
                        rhs: HeapRhs::Thunk {
                            signature: SignatureId(0),
                            update: UpdatePolicy::Memoize,
                            captures: vec![],
                            body: 0,
                        },
                    },
                    HeapBinding {
                        id: ValueId(2),
                        rhs: HeapRhs::Thunk {
                            signature: SignatureId(0),
                            update: UpdatePolicy::Memoize,
                            captures: vec![],
                            body: 1,
                        },
                    },
                ]),
                body: 2,
            },
        ];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = 3;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(message)) if message.contains("ValueId(99)")
        ));
    }
}

struct Validator<'a> {
    wire: &'a WireProgram,
    limits: DecodeLimits,
    work: usize,
    top_values: BTreeSet<ValueId>,
    defined_values: BTreeSet<ValueId>,
}

impl<'a> Validator<'a> {
    fn new(wire: &'a WireProgram, limits: DecodeLimits) -> Self {
        Self {
            wire,
            limits,
            work: 0,
            top_values: BTreeSet::new(),
            defined_values: BTreeSet::new(),
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
            self.check_rep(global.rep)?;
            if let Some(signature) = global.entry_signature {
                self.check_signature(signature)?;
                if global.rep != RuntimeRep::LiftedRef {
                    return Err(ParseError::InvalidSignature(
                        "only a lifted global can have a callable entry".into(),
                    ));
                }
            }
        }

        let mut constructor_symbols = BTreeSet::new();
        let mut family_sizes = BTreeMap::new();
        let mut family_tags = BTreeSet::new();
        for constructor in &self.wire.constructors {
            self.bump_work(constructor.field_reps.len() + 1)?;
            self.check_symbol(&constructor.identity)?;
            self.check_symbol(&constructor.family)?;
            if constructor.tag == 0 || constructor.tag > constructor.family_size {
                return Err(ParseError::InvalidLayout(
                    "constructor tag must be within its nonempty GHC family".into(),
                ));
            }
            if family_sizes
                .insert(&constructor.family, constructor.family_size)
                .is_some_and(|size| size != constructor.family_size)
            {
                return Err(ParseError::InvalidLayout(
                    "inconsistent constructor family size".into(),
                ));
            }
            if !family_tags.insert((&constructor.family, constructor.tag)) {
                return Err(ParseError::DuplicateDefinition(
                    "constructor tag within family".into(),
                ));
            }
            if !matches!(
                constructor.result_rep,
                RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef
            ) {
                return Err(ParseError::InvalidSignature(
                    "heap constructor requires a managed result representation".into(),
                ));
            }
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

        if self.wire.expressions.nodes.len() > self.limits.max_nodes {
            return Err(ParseError::LimitExceeded("nodes"));
        }
        check_flat_tree(&self.wire.expressions, &self.wire.bindings)?;

        self.walk_bindings(false)?;
        self.walk_bindings(true)?;
        Ok(())
    }

    fn binding_type<B>(&self, binding: &HeapBinding<B>) -> ValueType {
        match binding.rhs {
            HeapRhs::Bytes(_) => ValueType {
                rep: RuntimeRep::Address,
                callable: None,
            },
            HeapRhs::Function { signature, .. } | HeapRhs::Thunk { signature, .. } => ValueType {
                rep: RuntimeRep::LiftedRef,
                callable: Some(signature),
            },
            HeapRhs::Constructor { constructor, .. } => ValueType {
                rep: self.wire.constructors[constructor.0 as usize].result_rep,
                callable: None,
            },
        }
    }

    fn walk_bindings(&mut self, typed: bool) -> Result<(), ParseError> {
        let wire = self.wire;
        let mut walker = Walker::new(self, &wire.expressions, typed)?;
        for group in &wire.bindings {
            match group {
                Group::NonRecursive(binding) => {
                    walker.walk_top_binding(&binding.binding)?;
                    walker.publish_top(&binding.binding)?;
                }
                Group::Recursive(bindings) => {
                    for binding in bindings {
                        walker.publish_top(&binding.binding)?;
                    }
                    for binding in bindings {
                        walker.walk_top_binding(&binding.binding)?;
                    }
                }
            }
        }
        Ok(())
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
        let index = usize::try_from(binding.binding.id.0)
            .map_err(|_| ParseError::LimitExceeded("value ids"))?;
        if index >= self.limits.max_table_entries {
            return Err(ParseError::LimitExceeded("value ids"));
        }
        self.defined_values.insert(binding.binding.id);
        Ok(())
    }

    fn check_scalar(&mut self, literal: &ScalarLiteral) -> Result<(), ParseError> {
        match literal {
            ScalarLiteral::NullAddress => Ok(()),
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
        if !matches!(bits, 8 | 16 | 32 | 64) || bytes.len() != usize::from(bits) / 8 {
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
                matches!(bits, 8 | 16 | 32 | 64)
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

    fn bump_node(&mut self) -> Result<(), ParseError> {
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
