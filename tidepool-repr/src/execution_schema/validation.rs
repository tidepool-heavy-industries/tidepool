use std::collections::{BTreeMap, BTreeSet};

use super::{
    Alternative, AlternativePattern, Atom, CaseKind, CheckedLayout, ConstructorDecl, ConstructorId,
    DecodeLimits, Expr, ExprFrame, GlobalId, Group, HeapBinding, HeapRhs, JoinBinding, JoinId,
    OperationId, ParseError, ProgramRequirements, ResultContract, RuntimeRep, ScalarLiteral,
    SignatureId, SymbolIdentity, TypeNode, TypeNodeId, ValueId, ValueRef, WireProgram,
    EXECUTION_ABI_VERSION, SCHEMA_VERSION, SYNTHETIC_SITE_BIT,
};
use recursion::{try_expand_and_collapse, MappableFrame, PartiallyApplied};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy)]
struct ValueType {
    rep: RuntimeRep,
    callable: Option<SignatureId>,
}

// Diagnostics run a node's own checks, then its children in source order.
// The recursion driver's LIFO stack needs children stored in reverse order.
struct OrderedFrame<A> {
    children: Vec<A>,
    index: usize,
    mark: usize,
    expected: Option<Rc<ResultContract>>,
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

fn for_each_child<E>(
    frame: &ExprFrame<usize>,
    mut visit: impl FnMut(usize) -> Result<(), E>,
) -> Result<(), E> {
    match frame {
        ExprFrame::Return(_)
        | ExprFrame::Enter { .. }
        | ExprFrame::Call { .. }
        | ExprFrame::Operation { .. }
        | ExprFrame::Construct { .. }
        | ExprFrame::Jump { .. } => {}
        ExprFrame::Case {
            scrutinee,
            alternatives,
            ..
        } => {
            visit(*scrutinee)?;
            for alternative in alternatives {
                visit(alternative.body)?;
            }
        }
        ExprFrame::Let { bindings, body } => {
            let mut visit_binding = |binding: &HeapBinding| match &binding.rhs {
                HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } => visit(*body),
                HeapRhs::Bytes(_) | HeapRhs::Constructor { .. } => Ok(()),
            };
            match bindings {
                Group::NonRecursive(binding) => visit_binding(binding)?,
                Group::Recursive(bindings) => {
                    for binding in bindings {
                        visit_binding(binding)?;
                    }
                }
            }
            visit(*body)?;
        }
        ExprFrame::LetJoins { bindings, body } => {
            match bindings {
                Group::NonRecursive(binding) => visit(binding.body)?,
                Group::Recursive(bindings) => {
                    for binding in bindings {
                        visit(binding.body)?;
                    }
                }
            }
            visit(*body)?;
        }
    }
    Ok(())
}

fn check_flat_tree(tree: &Expr, bindings: &[Group<super::TopBinding>]) -> Result<(), ParseError> {
    let len = tree.nodes.len();
    let mut parents = vec![0u8; len];
    for (index, frame) in tree.nodes.iter().enumerate() {
        for_each_child(frame, |child| {
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
            Ok(())
        })?;
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
    closure_epoch: u64,
}

enum Undo {
    Value(usize, Option<ScopedValue>),
    Join(usize, Option<ScopedJoin>),
    Epoch(u64),
    JoinEpoch(u64),
}

#[derive(Clone)]
enum Action {
    FreshJoins,
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
    group_actions: Option<Rc<[Action]>>,
    actions: Vec<Action>,
    expected: Option<Rc<ResultContract>>,
}

struct Walker<'w, 'p> {
    validator: &'w mut Validator<'p>,
    tree: &'w Expr,
    values: Vec<Option<ScopedValue>>,
    joins: Vec<Option<ScopedJoin>>,
    undo: Vec<Undo>,
    epoch: u64,
    next_epoch: u64,
    join_epoch: u64,
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
            join_epoch: 1,
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
        let Some(entry) = self.joins.get(index).and_then(|entry| *entry) else {
            return Ok(None);
        };
        // A bottoming join cannot return through the wrong case continuation,
        // but even bottoming jumps must remain inside their heap closure.
        let visible = entry.closure_epoch == self.epoch
            && (entry.epoch == self.join_epoch
                || self.validator.signature(entry.signature)?.results == ResultContract::NoSuccess);
        Ok(visible.then_some(entry.signature))
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
            ty: self.validator.binding_type(binding)?,
            epoch: 0,
        });
        Ok(())
    }

    fn hide_top(&mut self, binding: &HeapBinding) -> Result<(), ParseError> {
        let index = self.value_index(binding.id)?;
        self.ensure_value(index);
        let old = self.values[index].take();
        self.undo.push(Undo::Value(index, old));
        Ok(())
    }

    fn bind_join(&mut self, id: JoinId, signature: SignatureId) -> Result<(), ParseError> {
        let index = self.join_index(id)?;
        self.ensure_join(index);
        let old = self.joins[index].replace(ScopedJoin {
            signature,
            epoch: self.join_epoch,
            closure_epoch: self.epoch,
        });
        self.undo.push(Undo::Join(index, old));
        Ok(())
    }

    fn restore(&mut self, mark: usize) {
        while self.undo.len() > mark {
            let Some(undo) = self.undo.pop() else {
                break;
            };
            match undo {
                Undo::Value(index, old) => self.values[index] = old,
                Undo::Join(index, old) => self.joins[index] = old,
                Undo::Epoch(epoch) => self.epoch = epoch,
                Undo::JoinEpoch(epoch) => self.join_epoch = epoch,
            }
        }
    }

    fn fresh_joins(&mut self) -> Result<(), ParseError> {
        self.undo.push(Undo::JoinEpoch(self.join_epoch));
        self.next_epoch = self
            .next_epoch
            .checked_add(1)
            .ok_or(ParseError::LimitExceeded("scope epochs"))?;
        self.join_epoch = self.next_epoch;
        Ok(())
    }

    fn apply(&mut self, actions: &[Action]) -> Result<(), ParseError> {
        for action in actions {
            match action {
                Action::FreshJoins => self.fresh_joins()?,
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
                    self.fresh_joins()?;
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
        if enter_only && (!signature.arguments.is_empty() || signature.results.is_caller_result()) {
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
                std::cmp::Ordering::Less
                    if signature.results
                        != super::ResultContract::Returns(vec![RuntimeRep::LiftedRef]) =>
                {
                    return Err(ParseError::InvalidSignature(
                        "partial application must return a function reference".into(),
                    ))
                }
                std::cmp::Ordering::Equal
                    if !(actual.results.satisfies(&signature.results)
                        || (actual.results.is_caller_result()
                            && matches!(signature.results, ResultContract::Returns(_)))) =>
                {
                    return Err(ParseError::InvalidSignature(
                        "saturated application result disagrees with entry signature".into(),
                    ))
                }
                std::cmp::Ordering::Greater
                    if actual.results != super::ResultContract::NoSuccess
                        && actual.results
                            != super::ResultContract::Returns(vec![RuntimeRep::LiftedRef]) =>
                {
                    return Err(ParseError::InvalidSignature(
                        "oversaturation requires a returned function reference".into(),
                    ))
                }
                _ => {}
            }
            if signature.arguments.len() > actual.arguments.len()
                && signature.results == ResultContract::NoSuccess
                && actual.results != ResultContract::NoSuccess
            {
                return Err(ParseError::InvalidSignature(
                    "oversaturated call cannot infer a nonreturning suffix".into(),
                ));
            }
            // Preserve independently established nonreturning evidence in the
            // expression result, even if its continuation demanded normal reps.
            if signature.arguments.len() >= actual.arguments.len()
                && actual.results == super::ResultContract::NoSuccess
            {
                return Ok(super::Signature {
                    arguments: signature.arguments,
                    results: super::ResultContract::NoSuccess,
                });
            }
        } else if signature.results == super::ResultContract::NoSuccess {
            return Err(ParseError::InvalidSignature(
                "unknown callee cannot establish a nonreturning result".into(),
            ));
        } else if ty.rep != RuntimeRep::LiftedRef
            && (!signature.arguments.is_empty()
                || signature.results != super::ResultContract::Returns(vec![ty.rep]))
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
        group_actions: Option<Rc<[Action]>>,
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
                    group_actions,
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
                if !signature.arguments.is_empty() || signature.results.is_caller_result() {
                    return Err(ParseError::InvalidSignature(
                        "thunk requires a concrete zero-argument signature".into(),
                    ));
                }
                actions.push(Action::Closure {
                    captures: captures.clone(),
                    parameters: Vec::new(),
                });
                Ok(Some(Seed {
                    index: body,
                    group_actions,
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
        let seed = self.rhs_seed(&binding.rhs, body, None, Vec::new())?;
        self.walk(seed)
    }

    fn expand(&mut self, seed: Seed) -> Result<OrderedFrame<Seed>, ParseError> {
        let mark = self.undo.len();
        if let Some(actions) = &seed.group_actions {
            self.apply(actions)?;
        }
        self.apply(&seed.actions)?;
        let tree = self.tree;
        // check_flat_tree has already verified every reachable child index.
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
                scrutinee_results,
                kind,
                alternatives,
            } => {
                children.push(Seed {
                    index: *scrutinee,
                    group_actions: None,
                    actions: vec![Action::FreshJoins],
                    expected: Some(Rc::new(scrutinee_results.clone())),
                });
                self.case_children(
                    *binder,
                    scrutinee_results,
                    kind,
                    alternatives,
                    &mut children,
                )
                .map_err(|error| match error {
                    ParseError::InvalidSignature(detail) => ParseError::InvalidSignature(format!(
                        "{detail}; scrutinee {}",
                        self.describe_node(*scrutinee)
                    )),
                    other => other,
                })?;
            }
            ExprFrame::Let { bindings, body } => {
                let actions = self.local_children(bindings, &mut children)?;
                children.push(Seed {
                    index: *body,
                    group_actions: Some(actions),
                    actions: Vec::new(),
                    expected: seed.expected.clone(),
                });
            }
            ExprFrame::LetJoins { bindings, body } => {
                let actions = self.join_children(bindings, &mut children)?;
                children.push(Seed {
                    index: *body,
                    group_actions: Some(actions),
                    actions: Vec::new(),
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
        mut frame: OrderedFrame<ResultContract>,
    ) -> Result<ResultContract, ParseError> {
        frame.children.reverse();
        let tree = self.tree;
        let actual = if self.typed {
            match &tree.nodes[frame.index] {
                ExprFrame::Return(atoms) => ResultContract::Returns(self.atom_reps(atoms)?),
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
                    ResultContract::Returns(vec![declaration.result_rep])
                }
                ExprFrame::Jump { join, arguments } => {
                    let signature_id = self.join(*join)?.ok_or_else(|| {
                        ParseError::InvalidScope(format!("join {:?} is out of scope", join))
                    })?;
                    let signature = self.validator.signature(signature_id)?.clone();
                    self.check_atom_reps(arguments, &signature.arguments, "join arguments")?;
                    signature.results
                }
                ExprFrame::Case { alternatives, .. } => {
                    let mut children = frame.children.into_iter();
                    let scrutinee = children.next().ok_or_else(|| {
                        ParseError::InvalidReference("case scrutinee result is missing".into())
                    })?;
                    let mut result = ResultContract::NoSuccess;
                    if !alternatives.is_empty() {
                        for child in children {
                            result = result.merge_alternative(&child).ok_or_else(|| {
                                ParseError::InvalidSignature(
                                    "case alternatives return different representations".into(),
                                )
                            })?;
                        }
                    }
                    if scrutinee == ResultContract::NoSuccess {
                        ResultContract::NoSuccess
                    } else {
                        result
                    }
                }
                ExprFrame::Let { .. } | ExprFrame::LetJoins { .. } => {
                    let result = frame.children.pop().ok_or_else(|| {
                        ParseError::InvalidReference("let body result is missing".into())
                    })?;
                    if let ExprFrame::LetJoins { bindings, .. } = &tree.nodes[frame.index] {
                        let expected = frame.expected.as_deref().unwrap_or(&result);
                        let bindings = match bindings {
                            Group::NonRecursive(binding) => std::slice::from_ref(binding),
                            Group::Recursive(bindings) => bindings.as_slice(),
                        };
                        for binding in bindings {
                            // A terminal context has no successful return ABI.
                            // It may retain an unused returning join; an actual
                            // jump to that join still fails the body's result
                            // check if it can return successfully.
                            if *expected != ResultContract::NoSuccess
                                && !self
                                    .validator
                                    .signature(binding.signature)?
                                    .results
                                    .satisfies(expected)
                            {
                                return Err(ParseError::InvalidSignature(
                                    "join results do not match the binding continuation".into(),
                                ));
                            }
                        }
                    }
                    result
                }
            }
        } else {
            ResultContract::Returns(Vec::new())
        };
        if self.typed {
            if let Some(expected) = frame.expected {
                if !actual.satisfies_physically(&expected) {
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
    ) -> Result<Rc<[Action]>, ParseError> {
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
                if let Some(seed) = self.rhs_seed(&binding.rhs, body, None, Vec::new())? {
                    children.push(seed);
                }
                Ok(Rc::from(vec![Action::Value(
                    binding.id,
                    self.validator.binding_type(binding)?,
                )]))
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
                        self.validator.binding_type(binding)?,
                    ));
                }
                let actions: Rc<[Action]> = Rc::from(actions);
                let mark = self.undo.len();
                self.apply(&actions)?;
                for binding in bindings {
                    let body = match &binding.rhs {
                        HeapRhs::Function { body, .. } | HeapRhs::Thunk { body, .. } => *body,
                        _ => 0,
                    };
                    if let Some(seed) =
                        self.rhs_seed(&binding.rhs, body, Some(actions.clone()), Vec::new())?
                    {
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
    ) -> Result<Rc<[Action]>, ParseError> {
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
        let actions: Rc<[Action]> = Rc::from(actions);
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
            let mut child_actions = Vec::new();
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
                group_actions: matches!(bindings, Group::Recursive(_)).then(|| actions.clone()),
                actions: child_actions,
                expected: Some(Rc::new(signature.results)),
            });
        }
        Ok(actions)
    }

    /// A short rendering of one expression node for diagnostics: operation
    /// nodes name their operation.
    fn describe_node(&self, index: usize) -> String {
        match self.tree.nodes.get(index) {
            Some(ExprFrame::Operation { operation, .. }) => format!(
                "operation {:?}",
                self.validator
                    .wire
                    .operations
                    .get(operation.0 as usize)
                    .map(|declaration| &declaration.identity)
            ),
            Some(frame) => {
                let rendered = format!("{frame:?}");
                rendered.chars().take(200).collect()
            }
            None => "missing".into(),
        }
    }

    fn case_children(
        &mut self,
        binder: ValueId,
        scrutinee_results: &ResultContract,
        kind: &CaseKind,
        alternatives: &[Alternative],
        children: &mut Vec<Seed>,
    ) -> Result<(), ParseError> {
        let scrutinee_reps = match scrutinee_results {
            ResultContract::CallerResult => {
                return Err(ParseError::InvalidSignature(
                    "case scrutinee requires concrete results".into(),
                ))
            }
            ResultContract::Returns(reps) => {
                for rep in reps {
                    self.validator.check_rep(*rep)?;
                }
                Some(reps.as_slice())
            }
            ResultContract::NoSuccess if alternatives.is_empty() => None,
            ResultContract::NoSuccess => {
                return Err(ParseError::InvalidSignature(
                    "case with alternatives requires returning scrutinee representations".into(),
                ));
            }
        };
        match kind {
            CaseKind::Algebraic(family) => {
                self.validator.check_symbol(family)?;
                if scrutinee_reps.is_some_and(|reps| {
                    !matches!(reps, [RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef])
                }) {
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
                ) || scrutinee_reps
                    .is_some_and(|reps| !matches!(reps, [actual] if actual.same_bits(*rep)))
                {
                    return Err(ParseError::InvalidSignature(format!(
                        "primitive case scrutinee representation mismatch: case binder {binder:?} is {rep:?}, scrutinee returns {scrutinee_reps:?}"
                    )));
                }
            }
            CaseKind::MultiValue | CaseKind::Polymorphic => {
                if !alternatives.is_empty()
                    && (alternatives.len() != 1
                        || alternatives[0].pattern != AlternativePattern::Default)
                {
                    return Err(ParseError::Malformed(
                        "multi-value and polymorphic cases require one DEFAULT alternative".into(),
                    ));
                }
                if *kind == CaseKind::MultiValue
                    && scrutinee_reps.is_some_and(|reps| reps.contains(&RuntimeRep::Void))
                {
                    return Err(ParseError::InvalidSignature(
                        "multi-value case contains a nonphysical Void component".into(),
                    ));
                }
            }
        }
        let mut case_actions = Vec::new();
        if self.value(binder)?.is_some() {
            return Err(ParseError::DuplicateDefinition(format!(
                "case binder {:?}",
                binder
            )));
        }
        self.register_value(binder)?;
        if alternatives.is_empty() {
            return Ok(());
        }
        if *kind != CaseKind::MultiValue {
            if let Some([rep]) = scrutinee_reps {
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
                (_, CaseKind::MultiValue) => scrutinee_reps.unwrap_or_default().to_vec(),
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
                group_actions: None,
                actions,
                expected: None,
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
        if self.wire.types.len() > self.limits.max_type_nodes {
            return Err(ParseError::LimitExceeded("type nodes"));
        }
        if self.wire.sites.len() > self.limits.max_sites {
            return Err(ParseError::LimitExceeded("sites"));
        }
        if self.wire.verb_sites.len() > self.limits.max_sites {
            return Err(ParseError::LimitExceeded("verb sites"));
        }

        for signature in &self.wire.signatures {
            self.bump_work(
                signature.arguments.len()
                    + signature
                        .results
                        .returned_reps()
                        .map_or(0, <[RuntimeRep]>::len)
                    + 1,
            )?;
            for rep in signature
                .arguments
                .iter()
                .chain(signature.results.returned_reps().unwrap_or(&[]))
            {
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
        let mut constructor_host_ids = BTreeSet::new();
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
                .insert(constructor.family.clone(), constructor.family_size)
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
            if !constructor_host_ids.insert(constructor.host_id) {
                return Err(ParseError::DuplicateDefinition(
                    "constructor host id".into(),
                ));
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

        if let Some(layout) = &self.wire.json_layout {
            self.check_json_layout(layout)?;
        }

        self.check_type_nodes(&family_sizes)?;
        self.check_sites()?;
        self.check_verb_sites()?;

        let mut operation_contracts = BTreeSet::new();
        for operation in &self.wire.operations {
            if self
                .signature(operation.signature)?
                .results
                .is_caller_result()
            {
                return Err(ParseError::InvalidSignature(
                    "operation requires concrete results".into(),
                ));
            }
            self.bump_work(1)?;
            self.check_operation_identity(&operation.identity)?;
            let signature = self.signature(operation.signature)?;
            match &operation.identity {
                super::OperationIdentity::Capability { .. }
                    if matches!(signature.results, ResultContract::NoSuccess) =>
                {
                    return Err(ParseError::InvalidSignature(
                        "capability operation must have a successful result contract".into(),
                    ));
                }
                super::OperationIdentity::WiredInError { kind } => {
                    let arguments = if *kind == super::WiredInErrorKind::AbsentSumField {
                        &[][..]
                    } else {
                        &[RuntimeRep::Address][..]
                    };
                    if signature.arguments != arguments
                        || signature.results != ResultContract::NoSuccess
                    {
                        return Err(ParseError::InvalidSignature(format!(
                            "wired-in error {kind:?} must have signature {arguments:?} -> NoSuccess"
                        )));
                    }
                }
                _ => {}
            }
            let key = (
                operation.identity.clone(),
                signature.arguments.clone(),
                signature.results.clone(),
            );
            if !operation_contracts.insert(key) {
                return Err(ParseError::DuplicateDefinition(format!(
                    "operation {:?}",
                    operation.identity
                )));
            }
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

        for group in &self.wire.bindings {
            let tops = match group {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops,
            };
            for top in tops {
                if top.binding.id == self.wire.entry {
                    if let HeapRhs::Function { signature, .. } = &top.binding.rhs {
                        if self.signature(*signature)?.results.is_caller_result() {
                            return Err(ParseError::InvalidSignature(
                                "program entry requires concrete results".into(),
                            ));
                        }
                    }
                }
            }
        }

        if self.wire.expressions.nodes.len() > self.limits.max_nodes {
            return Err(ParseError::LimitExceeded("nodes"));
        }
        check_flat_tree(&self.wire.expressions, &self.wire.bindings)?;

        self.walk_bindings(false)?;
        self.walk_bindings(true)?;
        Ok(())
    }

    fn binding_type<B>(&self, binding: &HeapBinding<B>) -> Result<ValueType, ParseError> {
        Ok(match &binding.rhs {
            HeapRhs::Bytes(_) => ValueType {
                rep: RuntimeRep::Address,
                callable: None,
            },
            HeapRhs::Function { signature, .. } | HeapRhs::Thunk { signature, .. } => ValueType {
                rep: RuntimeRep::LiftedRef,
                callable: Some(*signature),
            },
            HeapRhs::Constructor { constructor, .. } => ValueType {
                rep: self.constructor(*constructor)?.result_rep,
                callable: None,
            },
        })
    }

    fn walk_bindings(&mut self, typed: bool) -> Result<(), ParseError> {
        let wire = self.wire;
        let mut walker = Walker::new(self, &wire.expressions, typed)?;
        // Top declaration metadata is published before any RHS walks. Errors
        // in this phase intentionally precede RHS errors; source order applies
        // to the subsequent walks.
        for group in &wire.bindings {
            match group {
                Group::NonRecursive(binding) => walker.publish_top(&binding.binding)?,
                Group::Recursive(bindings) => {
                    for binding in bindings {
                        walker.publish_top(&binding.binding)?;
                    }
                }
            }
        }
        for group in &wire.bindings {
            match group {
                Group::NonRecursive(binding) => {
                    let mark = walker.undo.len();
                    walker.hide_top(&binding.binding)?;
                    let result = walker.walk_top_binding(&binding.binding);
                    walker.restore(mark);
                    result?;
                }
                Group::Recursive(bindings) => {
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
        for rep in reps {
            self.check_rep(*rep)?;
        }
        let expected =
            crate::execution_schema::StorageLayout::for_reps(&self.wire.envelope.target, reps)
                .map_err(|error| ParseError::InvalidLayout(error.to_string()))?;
        if layout.fields.len() != expected.fields().len()
            || layout.root_mask.len() != expected.fields().len()
        {
            return Err(ParseError::InvalidLayout(
                "stored fields/layout/root mask length mismatch".into(),
            ));
        }

        for (field, expected_field) in layout.fields.iter().zip(expected.fields()) {
            if field.rep != expected_field.rep() || field.offset != expected_field.offset() {
                return Err(ParseError::InvalidLayout(
                    "layout field does not match canonical storage layout".into(),
                ));
            }
        }
        let expected_root_mask: Vec<_> = expected
            .fields()
            .iter()
            .map(|field| matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef))
            .collect();
        if layout.root_mask != expected_root_mask {
            return Err(ParseError::InvalidLayout(
                "incorrect canonical layout root mask".into(),
            ));
        }
        if layout.alignment != expected.alignment()
            || layout.payload_size != expected.payload_size()
        {
            return Err(ParseError::InvalidLayout(
                "layout payload size is inconsistent".into(),
            ));
        }
        Ok(())
    }

    fn check_type_nodes(
        &mut self,
        family_sizes: &BTreeMap<SymbolIdentity, u32>,
    ) -> Result<(), ParseError> {
        for index in 0..self.wire.types.len() {
            let work = match &self.wire.types[index] {
                TypeNode::Data {
                    family,
                    arguments,
                    rows,
                } => arguments
                    .len()
                    .checked_add(rows.len())
                    .and_then(|work| {
                        rows.iter()
                            .try_fold(work, |work, row| work.checked_add(row.fields.len()))
                    })
                    .and_then(|work| {
                        Self::symbol_text_len(family).and_then(|text| work.checked_add(text))
                    }),
                TypeNode::Unconstructible { reason, rendered } => {
                    reason.len().checked_add(rendered.len())
                }
                TypeNode::Text | TypeNode::Integer | TypeNode::Natural | TypeNode::Scalar(_) => {
                    Some(0)
                }
            }
            .and_then(|work| work.checked_add(1))
            .ok_or(ParseError::LimitExceeded("work"))?;
            self.bump_work(work)?;
            match &self.wire.types[index] {
                TypeNode::Data {
                    family,
                    arguments,
                    rows,
                } => {
                    self.check_symbol_shape(family)?;
                    for argument in arguments {
                        self.type_node(*argument)?;
                    }
                    if rows.is_empty() {
                        if family_sizes.contains_key(family) {
                            return Err(ParseError::InvalidLayout(
                                "type node constructor family size".into(),
                            ));
                        }
                        continue;
                    }
                    let family_size = self.constructor(rows[0].constructor)?.family_size;
                    if rows.len() != family_size as usize {
                        return Err(ParseError::InvalidLayout(
                            "type node constructor family size".into(),
                        ));
                    }
                    for (row_index, row) in rows.iter().enumerate() {
                        let constructor = self.constructor(row.constructor)?;
                        let expected_tag = u32::try_from(row_index + 1)
                            .map_err(|_| ParseError::LimitExceeded("type nodes"))?;
                        if &constructor.family != family
                            || constructor.family_size != family_size
                            || constructor.tag != expected_tag
                        {
                            return Err(ParseError::InvalidLayout(
                                "type node constructor family".into(),
                            ));
                        }
                        if constructor.result_rep != RuntimeRep::LiftedRef {
                            return Err(ParseError::InvalidLayout(
                                "type node constructor representation".into(),
                            ));
                        }
                        if row.fields.len() != constructor.field_reps.len() {
                            return Err(ParseError::InvalidLayout(
                                "type node field representation".into(),
                            ));
                        }
                        for (field, expected_rep) in row.fields.iter().zip(&constructor.field_reps)
                        {
                            match self.type_node(*field)? {
                                TypeNode::Scalar(rep) if rep == expected_rep => {}
                                TypeNode::Data { .. }
                                | TypeNode::Text
                                | TypeNode::Integer
                                | TypeNode::Natural
                                    if *expected_rep == RuntimeRep::LiftedRef => {}
                                TypeNode::Unconstructible { .. } => {}
                                _ => {
                                    return Err(ParseError::InvalidLayout(
                                        "type node field representation".into(),
                                    ));
                                }
                            }
                        }
                    }
                }
                TypeNode::Scalar(rep) => {
                    self.check_rep(*rep)?;
                    if !matches!(
                        rep,
                        RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_)
                    ) {
                        return Err(ParseError::InvalidLayout(
                            "type scalar representation".into(),
                        ));
                    }
                }
                TypeNode::Unconstructible { reason, rendered } => {
                    if reason.is_empty() {
                        return Err(ParseError::Malformed("empty unconstructible reason".into()));
                    }
                    self.check_text_shape(reason, true)?;
                    self.check_text_shape(rendered, true)?;
                }
                TypeNode::Text | TypeNode::Integer | TypeNode::Natural => {}
            }
        }
        Ok(())
    }

    fn check_sites(&mut self) -> Result<(), ParseError> {
        let mut ids = BTreeSet::new();
        for index in 0..self.wire.sites.len() {
            let work = self.wire.sites[index]
                .inputs
                .len()
                .checked_add(self.wire.sites[index].origin.len())
                .and_then(|work| work.checked_add(1))
                .ok_or(ParseError::LimitExceeded("work"))?;
            self.bump_work(work)?;
            let site = &self.wire.sites[index];
            if site.site == 0 {
                return Err(ParseError::InvalidReference("site 0".into()));
            }
            if !ids.insert(site.site) {
                return Err(ParseError::DuplicateDefinition("site".into()));
            }
            self.check_text_shape(&site.origin, true)?;
            self.type_node(site.wire)?;
            for input in &site.inputs {
                self.type_node(*input)?;
            }
        }
        Ok(())
    }

    /// Each verb-site entry names a declared constructor (at most once) and
    /// an admitted synthetic row; every synthetic row is named by one. With
    /// dynamic ids never carrying [`SYNTHETIC_SITE_BIT`], the two site ranges
    /// cannot collide.
    fn check_verb_sites(&mut self) -> Result<(), ParseError> {
        let mut constructors = BTreeSet::new();
        let mut named = BTreeSet::new();
        for index in 0..self.wire.verb_sites.len() {
            self.bump_work(1)?;
            let (constructor, site) = self.wire.verb_sites[index];
            if constructor.0 as usize >= self.wire.constructors.len() {
                return Err(ParseError::InvalidReference(format!(
                    "verb site constructor {constructor:?}"
                )));
            }
            if !constructors.insert(constructor) {
                return Err(ParseError::DuplicateDefinition("verb site".into()));
            }
            if site & SYNTHETIC_SITE_BIT == 0 {
                return Err(ParseError::InvalidReference(format!(
                    "verb site {site} is not in the synthetic range"
                )));
            }
            if !self.wire.sites.iter().any(|row| row.site == site) {
                return Err(ParseError::InvalidReference(format!(
                    "verb site {site} names no site row"
                )));
            }
            named.insert(site);
        }
        if let Some(row) = self
            .wire
            .sites
            .iter()
            .find(|row| row.site & SYNTHETIC_SITE_BIT != 0 && !named.contains(&row.site))
        {
            return Err(ParseError::InvalidReference(format!(
                "synthetic site {} is named by no verb site",
                row.site
            )));
        }
        Ok(())
    }

    fn type_node(&self, id: TypeNodeId) -> Result<&TypeNode, ParseError> {
        self.wire
            .types
            .get(id.0 as usize)
            .ok_or_else(|| ParseError::InvalidReference(format!("type node {:?}", id)))
    }

    fn check_text_shape(&self, text: &str, allow_empty: bool) -> Result<(), ParseError> {
        if !allow_empty && text.is_empty() {
            return Err(ParseError::Malformed("empty identity text".into()));
        }
        if text.len() > self.limits.max_string_bytes {
            return Err(ParseError::LimitExceeded("string bytes"));
        }
        Ok(())
    }

    fn check_symbol_shape(&self, symbol: &SymbolIdentity) -> Result<(), ParseError> {
        self.check_text_shape(&symbol.unit, false)?;
        self.check_text_shape(&symbol.module, false)?;
        self.check_text_shape(&symbol.namespace, false)?;
        self.check_text_shape(&symbol.occurrence, false)?;
        if let Some(parent) = &symbol.record_parent {
            self.check_text_shape(parent, false)?;
        }
        Ok(())
    }

    fn symbol_text_len(symbol: &SymbolIdentity) -> Option<usize> {
        [
            symbol.unit.len(),
            symbol.module.len(),
            symbol.namespace.len(),
            symbol.occurrence.len(),
            symbol.record_parent.as_ref().map_or(0, String::len),
        ]
        .into_iter()
        .try_fold(0, usize::checked_add)
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
        self.check_text(&symbol.occurrence)?;
        if let Some(parent) = &symbol.record_parent {
            self.check_text(parent)?;
        }
        Ok(())
    }

    fn check_operation_identity(
        &mut self,
        identity: &super::OperationIdentity,
    ) -> Result<(), ParseError> {
        match identity {
            super::OperationIdentity::PrimOp(name) => self.check_text(name),
            super::OperationIdentity::Intrinsic { symbol, .. } => self.check_text(symbol),
            super::OperationIdentity::JsonDecode { left, right } => {
                self.require_json_layout()?;
                self.check_json_decode_result(*left, *right)
            }
            super::OperationIdentity::JsonEncode => self.require_json_layout(),
            super::OperationIdentity::Capability { name } => self.check_text(name),
            super::OperationIdentity::WiredInError { .. } => Ok(()),
        }
    }

    fn require_json_layout(&self) -> Result<(), ParseError> {
        self.wire.json_layout.as_ref().ok_or_else(|| {
            ParseError::Malformed("JSON operation lacks program layout evidence".into())
        })?;
        Ok(())
    }

    fn json_constructor(
        &self,
        id: ConstructorId,
        role: &'static str,
        seen: &mut BTreeSet<ConstructorId>,
    ) -> Result<&ConstructorDecl, ParseError> {
        if !seen.insert(id) {
            return Err(ParseError::Malformed(format!(
                "duplicate JSON constructor role at {role}"
            )));
        }
        self.wire.constructors.get(id.0 as usize).ok_or_else(|| {
            ParseError::Malformed(format!("JSON {role} constructor is out of range"))
        })
    }

    fn check_json_reps(
        declaration: &ConstructorDecl,
        role: &'static str,
        expected: &[RuntimeRep],
    ) -> Result<(), ParseError> {
        if declaration.result_rep != RuntimeRep::LiftedRef || declaration.field_reps != expected {
            return Err(ParseError::InvalidLayout(format!(
                "JSON {role} constructor has an inadmissible physical layout"
            )));
        }
        Ok(())
    }

    fn check_json_same_family(
        role: &'static str,
        first: &ConstructorDecl,
        other: &ConstructorDecl,
    ) -> Result<(), ParseError> {
        if first.family != other.family || first.family_size != other.family_size {
            return Err(ParseError::Malformed(format!(
                "JSON {role} constructors do not share declared nominal family evidence"
            )));
        }
        Ok(())
    }

    /// JSON role assignment is compiler evidence, so schema admission checks
    /// that every named role has the representation and family relation the
    /// runtime can safely construct. It deliberately does not reconstruct a
    /// second module-name inventory.
    fn check_json_layout(&self, layout: &super::JsonLayout) -> Result<(), ParseError> {
        let mut seen = BTreeSet::new();
        let object = self.json_constructor(layout.object, "Object", &mut seen)?;
        let array = self.json_constructor(layout.array, "Array", &mut seen)?;
        let string = self.json_constructor(layout.string, "String", &mut seen)?;
        let number = self.json_constructor(layout.number, "Number", &mut seen)?;
        let bool_ = self.json_constructor(layout.bool_, "Bool", &mut seen)?;
        let null = self.json_constructor(layout.null, "Null", &mut seen)?;
        let map_bin = self.json_constructor(layout.map_bin, "map Bin", &mut seen)?;
        let map_tip = self.json_constructor(layout.map_tip, "map Tip", &mut seen)?;
        let true_ = self.json_constructor(layout.true_, "True", &mut seen)?;
        let false_ = self.json_constructor(layout.false_, "False", &mut seen)?;
        let cons = self.json_constructor(layout.cons, "list cons", &mut seen)?;
        let nil = self.json_constructor(layout.nil, "list nil", &mut seen)?;
        let scientific = self.json_constructor(layout.scientific, "Scientific", &mut seen)?;
        let integer_small =
            self.json_constructor(layout.integer_small, "integer small", &mut seen)?;
        let integer_positive =
            self.json_constructor(layout.integer_positive, "integer positive", &mut seen)?;
        let integer_negative =
            self.json_constructor(layout.integer_negative, "integer negative", &mut seen)?;
        let text = self.json_constructor(layout.text, "Text", &mut seen)?;
        let int = self.json_constructor(layout.int, "boxed Int", &mut seen)?;

        for (role, declaration) in [
            ("Object", object),
            ("Array", array),
            ("String", string),
            ("Number", number),
            ("Bool", bool_),
        ] {
            Self::check_json_reps(declaration, role, &[RuntimeRep::LiftedRef])?;
            Self::check_json_same_family("Value", object, declaration)?;
        }
        Self::check_json_reps(null, "Null", &[])?;
        Self::check_json_same_family("Value", object, null)?;
        Self::check_json_reps(map_tip, "map Tip", &[])?;
        Self::check_json_reps(
            map_bin,
            "map Bin",
            &[
                RuntimeRep::LiftedRef,
                RuntimeRep::LiftedRef,
                RuntimeRep::LiftedRef,
                RuntimeRep::LiftedRef,
                RuntimeRep::LiftedRef,
            ],
        )
        .or_else(|_| {
            Self::check_json_reps(
                map_bin,
                "map Bin",
                &[
                    RuntimeRep::Int(64),
                    RuntimeRep::LiftedRef,
                    RuntimeRep::LiftedRef,
                    RuntimeRep::LiftedRef,
                    RuntimeRep::LiftedRef,
                ],
            )
        })?;
        Self::check_json_same_family("Map", map_bin, map_tip)?;
        Self::check_json_reps(true_, "True", &[])?;
        Self::check_json_reps(false_, "False", &[])?;
        Self::check_json_same_family("Bool", true_, false_)?;
        Self::check_json_reps(
            cons,
            "list cons",
            &[RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
        )?;
        Self::check_json_reps(nil, "list nil", &[])?;
        Self::check_json_same_family("list", cons, nil)?;
        Self::check_json_reps(
            scientific,
            "Scientific",
            &[RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
        )
        .or_else(|_| {
            Self::check_json_reps(
                scientific,
                "Scientific",
                &[RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
            )
        })?;
        Self::check_json_reps(integer_small, "integer small", &[RuntimeRep::Int(64)])?;
        Self::check_json_reps(
            integer_positive,
            "integer positive",
            &[RuntimeRep::UnliftedRef],
        )?;
        Self::check_json_reps(
            integer_negative,
            "integer negative",
            &[RuntimeRep::UnliftedRef],
        )?;
        Self::check_json_same_family("Integer", integer_small, integer_positive)?;
        Self::check_json_same_family("Integer", integer_small, integer_negative)?;
        Self::check_json_reps(
            text,
            "Text",
            &[
                RuntimeRep::UnliftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
            ],
        )?;
        Self::check_json_reps(int, "boxed Int", &[RuntimeRep::Int(64)])?;
        Ok(())
    }

    fn check_json_decode_result(
        &self,
        left: ConstructorId,
        right: ConstructorId,
    ) -> Result<(), ParseError> {
        let mut seen = BTreeSet::new();
        let left = self.json_constructor(left, "decode Left", &mut seen)?;
        let right = self.json_constructor(right, "decode Right", &mut seen)?;
        Self::check_json_reps(left, "decode Left", &[RuntimeRep::LiftedRef])?;
        Self::check_json_reps(right, "decode Right", &[RuntimeRep::LiftedRef])?;
        Self::check_json_same_family("decode result", left, right)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_schema::{
        Architecture, ConstructorDecl, CtorRow, Endianness, FieldLayout, HeapBinding, HeapRhs,
        OperationDecl, ProgramEnvelope, Signature, SiteDelivery, SiteRow, TargetDescriptor,
        TopBinding, TypeNode, TypeNodeId, UpdatePolicy, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
    };

    fn symbol(name: &str) -> SymbolIdentity {
        SymbolIdentity {
            unit: "fixture".into(),
            module: "M3.Validation".into(),
            namespace: "value".into(),
            occurrence: name.into(),
            record_parent: None,
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
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
            types: vec![],
            sites: vec![],
            verb_sites: vec![],
            json_layout: None,
        }
    }

    #[test]
    fn json_operations_require_program_layout_evidence() {
        let program = valid_program();
        let mut validator = Validator::new(&program, DecodeLimits::default());
        let encode = super::super::OperationIdentity::JsonEncode;
        assert!(matches!(
            validator.check_operation_identity(&encode),
            Err(ParseError::Malformed(message)) if message.contains("lacks program layout evidence")
        ));

        let decode = super::super::OperationIdentity::JsonDecode {
            left: ConstructorId(0),
            right: ConstructorId(0),
        };
        assert!(matches!(
            validator.check_operation_identity(&decode),
            Err(ParseError::Malformed(message)) if message.contains("lacks program layout evidence")
        ));
    }

    fn replace_root(program: &mut WireProgram, frame: ExprFrame<usize>) {
        program.expressions.nodes = vec![frame];
    }

    #[test]
    fn no_success_is_established_by_callable_entry_signature() {
        let mut program = valid_program();
        program.globals.push(super::super::GlobalDecl {
            identity: symbol("bottom"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        });
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        program.globals[0].entry_signature = Some(SignatureId(0));
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        program.signatures.push(Signature {
            arguments: vec![RuntimeRep::Void],
            results: ResultContract::NoSuccess,
        });
        program.globals[0].entry_signature = Some(SignatureId(1));
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
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
            host_id: crate::DataConId(u64::from(tag)),
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
    fn constructor_host_ids_are_distinct_from_family_tags() {
        let mut program = valid_program();
        let mut other = empty_constructor("Other", 1, 1);
        other.family = symbol("OtherFamily");
        other.host_id = crate::DataConId(2);
        program.constructors = vec![empty_constructor("A", 1, 1), other];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();

        program.constructors[1].host_id = program.constructors[0].host_id;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition(message)) if message.contains("host id")
        ));
    }

    #[test]
    fn type_graph_keeps_phantom_arguments_distinct_and_allows_cycles() {
        let mut program = valid_program();
        let family = symbol("PhantomFamily");
        program.types = vec![
            TypeNode::Scalar(RuntimeRep::Int(64)),
            TypeNode::Scalar(RuntimeRep::Word(64)),
            TypeNode::Data {
                family: family.clone(),
                arguments: vec![TypeNodeId(0)],
                rows: vec![],
            },
            TypeNode::Data {
                family,
                arguments: vec![TypeNodeId(1)],
                rows: vec![],
            },
        ];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        assert_ne!(program.types[2], program.types[3]);

        let mut recursive = empty_constructor("Recursive", 1, 1);
        recursive.family = symbol("RecursiveFamily");
        recursive.field_reps = vec![RuntimeRep::LiftedRef];
        recursive.strict_fields = vec![false];
        recursive.layout = CheckedLayout {
            fields: vec![FieldLayout {
                rep: RuntimeRep::LiftedRef,
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![true],
        };
        program.constructors = vec![recursive];
        program.types = vec![TypeNode::Data {
            family: symbol("RecursiveFamily"),
            arguments: vec![],
            rows: vec![CtorRow {
                constructor: ConstructorId(0),
                fields: vec![TypeNodeId(0)],
            }],
        }];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn type_graph_rejects_bad_references_and_field_layouts() {
        let mut program = valid_program();
        program.types = vec![TypeNode::Data {
            family: symbol("Family"),
            arguments: vec![TypeNodeId(1)],
            rows: vec![],
        }];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("type node")
        ));

        let mut constructor = empty_constructor("Scalar", 1, 1);
        constructor.field_reps = vec![RuntimeRep::Int(64)];
        constructor.strict_fields = vec![false];
        constructor.layout = CheckedLayout {
            fields: vec![FieldLayout {
                rep: RuntimeRep::Int(64),
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![false],
        };
        program.constructors = vec![constructor];
        program.types = vec![
            TypeNode::Scalar(RuntimeRep::Word(64)),
            TypeNode::Data {
                family: symbol("Family"),
                arguments: vec![],
                rows: vec![CtorRow {
                    constructor: ConstructorId(0),
                    fields: vec![TypeNodeId(0)],
                }],
            },
        ];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(detail))
                if detail == "type node field representation"
        ));

        let TypeNode::Data { rows, .. } = &mut program.types[1] else {
            unreachable!()
        };
        rows[0].fields[0] = TypeNodeId(99);
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("type node")
        ));
    }

    #[test]
    fn site_rows_require_unique_nonzero_ids_and_valid_type_roots() {
        let mut program = valid_program();
        program.types = vec![TypeNode::Text];
        let site = SiteRow {
            site: 7,
            origin: "Fixture.hs:1".into(),
            ordinal: 0,
            delivery: SiteDelivery::HostAnswer,
            wire: TypeNodeId(0),
            inputs: vec![TypeNodeId(0)],
        };
        program.sites = vec![site.clone()];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();

        program.sites.push(site);
        assert_eq!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition("site".into()))
        );
        program.sites.truncate(1);
        program.sites[0].site = 0;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(_))
        ));
        program.sites[0].site = 7;
        program.sites[0].wire = TypeNodeId(1);
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("type node")
        ));
    }

    #[test]
    fn verb_sites_name_declared_constructors_and_synthetic_rows() {
        let mut program = valid_program();
        program.types = vec![TypeNode::Text];
        program.constructors = vec![empty_constructor("Print", 1, 1)];
        let synthetic = SYNTHETIC_SITE_BIT | 41;
        program.sites = vec![SiteRow {
            site: synthetic,
            origin: "Fixture.Print".into(),
            ordinal: 0,
            delivery: SiteDelivery::HostAnswer,
            wire: TypeNodeId(0),
            inputs: vec![],
        }];
        program.verb_sites = vec![(ConstructorId(0), synthetic)];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();

        // A synthetic row nobody names, and a verb site naming no row.
        program.verb_sites.clear();
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("named by no verb site")
        ));
        program.verb_sites = vec![(ConstructorId(0), SYNTHETIC_SITE_BIT | 42)];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("names no site row")
        ));
        // A dynamic id is never a verb site.
        program.sites[0].site = 41;
        program.verb_sites = vec![(ConstructorId(0), 41)];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("synthetic range")
        ));
        program.sites[0].site = synthetic;
        program.verb_sites = vec![(ConstructorId(1), synthetic)];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(detail)) if detail.contains("constructor")
        ));
        program.verb_sites = vec![(ConstructorId(0), synthetic), (ConstructorId(0), synthetic)];
        assert_eq!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition("verb site".into()))
        );
    }

    #[test]
    fn data_type_rows_cover_the_family_in_tag_order() {
        let mut program = valid_program();
        program.constructors = vec![
            empty_constructor("First", 1, 2),
            empty_constructor("Second", 2, 2),
        ];
        program.types = vec![TypeNode::Data {
            family: symbol("Family"),
            arguments: vec![],
            rows: vec![
                CtorRow {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
                CtorRow {
                    constructor: ConstructorId(1),
                    fields: vec![],
                },
            ],
        }];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();

        program.constructors[0].result_rep = RuntimeRep::UnliftedRef;
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(detail))
                if detail == "type node constructor representation"
        ));
        program.constructors[0].result_rep = RuntimeRep::LiftedRef;

        match &mut program.types[0] {
            TypeNode::Data { rows, .. } => rows.swap(0, 1),
            _ => unreachable!(),
        }
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
        match &mut program.types[0] {
            TypeNode::Data { rows, .. } => {
                rows.swap(0, 1);
                rows.remove(1);
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
        match &mut program.types[0] {
            TypeNode::Data { rows, .. } => rows.clear(),
            _ => unreachable!(),
        }
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
    }

    #[test]
    fn type_nodes_reject_non_scalar_representations_and_empty_refusal_reasons() {
        let mut program = valid_program();
        program.types = vec![TypeNode::Scalar(RuntimeRep::LiftedRef)];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidLayout(_))
        ));
        program.types = vec![TypeNode::Unconstructible {
            reason: String::new(),
            rendered: "Opaque".into(),
        }];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::Malformed(_))
        ));
    }

    #[test]
    fn type_and_site_tables_have_independent_semantic_limits() {
        let mut program = valid_program();
        program.types = vec![TypeNode::Text];
        assert_eq!(
            validate_program(
                &program,
                &requirements(),
                DecodeLimits {
                    max_type_nodes: 0,
                    ..DecodeLimits::default()
                }
            ),
            Err(ParseError::LimitExceeded("type nodes"))
        );
        program.sites = vec![SiteRow {
            site: 1,
            origin: String::new(),
            ordinal: 0,
            delivery: SiteDelivery::TerminalCapture,
            wire: TypeNodeId(0),
            inputs: vec![],
        }];
        assert_eq!(
            validate_program(
                &program,
                &requirements(),
                DecodeLimits {
                    max_sites: 0,
                    ..DecodeLimits::default()
                }
            ),
            Err(ParseError::LimitExceeded("sites"))
        );
    }

    #[test]
    fn many_empty_data_nodes_use_the_constructor_family_index() {
        let mut program = valid_program();
        for index in 0..4096_u32 {
            let mut constructor = empty_constructor(&format!("Constructor{index}"), 1, 1);
            constructor.host_id = crate::DataConId(u64::from(index) + 1);
            constructor.family = symbol(&format!("DeclaredFamily{index}"));
            program.constructors.push(constructor);
            program.types.push(TypeNode::Data {
                family: symbol(&format!("EmptyFamily{index}")),
                arguments: vec![],
                rows: vec![],
            });
        }
        validate_program(
            &program,
            &requirements(),
            DecodeLimits {
                max_work: 1 << 20,
                ..DecodeLimits::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn unsupported_scalar_widths_never_reach_native_layout() {
        for rep in [
            RuntimeRep::Int(128),
            RuntimeRep::Word(128),
            RuntimeRep::Int(24),
        ] {
            let mut program = valid_program();
            program.signatures[0].results = ResultContract::Returns(vec![rep]);
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

    fn case_join_program(join_inside_scrutinee: bool, result: RuntimeRep) -> WireProgram {
        let mut program = valid_program();
        program.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![result]),
        });
        let join = JoinBinding {
            id: JoinId(0),
            signature: SignatureId(1),
            parameters: vec![],
            body: 0,
        };
        let jump = ExprFrame::Jump {
            join: JoinId(0),
            arguments: vec![],
        };
        program.expressions.nodes = vec![ExprFrame::Return(vec![Atom::Rubbish(result)]), jump];
        let case = |scrutinee, body| ExprFrame::Case {
            scrutinee,
            binder: ValueId(1),
            scrutinee_results: ResultContract::Returns(vec![result]),
            kind: CaseKind::Primitive(result),
            alternatives: vec![Alternative {
                pattern: AlternativePattern::Default,
                binders: vec![],
                body,
            }],
        };
        if join_inside_scrutinee {
            program.expressions.nodes.push(ExprFrame::LetJoins {
                bindings: Group::NonRecursive(join),
                body: 1,
            });
            program
                .expressions
                .nodes
                .push(ExprFrame::Return(vec![integer(42)]));
            program.expressions.nodes.push(case(2, 3));
        } else {
            program
                .expressions
                .nodes
                .push(ExprFrame::Return(vec![integer(42)]));
            program.expressions.nodes.push(case(1, 2));
            program.expressions.nodes.push(ExprFrame::LetJoins {
                bindings: Group::NonRecursive(join),
                body: 3,
            });
        }
        let Group::NonRecursive(top) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut top.binding.rhs else {
            unreachable!()
        };
        *body = 4;
        program
    }

    #[test]
    fn case_scrutinee_cannot_jump_to_enclosing_continuation() {
        // Equal physical result types still skip the case alternative if the
        // jump escapes to the enclosing continuation.
        let program = case_join_program(false, RuntimeRep::Int(64));
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(_))
        ));
    }

    #[test]
    fn nonreturning_join_can_cross_a_case_but_not_a_heap_closure() {
        let mut program = case_join_program(false, RuntimeRep::Int(64));
        program.signatures[1].results = ResultContract::NoSuccess;
        // A recursive bottom join has no returning continuation to bypass.
        program.expressions.nodes[0] = ExprFrame::Jump {
            join: JoinId(0),
            arguments: vec![],
        };
        let ExprFrame::LetJoins { bindings, .. } = &mut program.expressions.nodes[4] else {
            unreachable!()
        };
        let Group::NonRecursive(join) = bindings.clone() else {
            unreachable!()
        };
        *bindings = Group::Recursive(vec![join]);
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();

        // Moving that same jump into a new thunk must remain invalid even
        // though the join is nonreturning: heap closures cannot capture joins.
        program.expressions.nodes[3] = ExprFrame::Let {
            bindings: Group::NonRecursive(HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(1),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: 1,
                },
            }),
            body: 2,
        };
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(_))
        ));
    }

    #[test]
    fn case_scrutinee_can_declare_its_own_join() {
        let program = case_join_program(true, RuntimeRep::Word(64));
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn unused_join_must_match_its_binding_continuation() {
        let mut program = case_join_program(false, RuntimeRep::Word(64));
        program.expressions.nodes[1] = ExprFrame::Return(vec![Atom::Rubbish(RuntimeRep::Word(64))]);
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidSignature(_))
        ));
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
            program.signatures[0].results = ResultContract::Returns(vec![rep]);
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
        program.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        program.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
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
            program.signatures[0].results = ResultContract::Returns(vec![rep]);
            replace_root(
                &mut program,
                ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]),
            );
            validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
            program.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
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

    fn callable_program(
        actual: Signature,
        application: Signature,
        arguments: Vec<Atom>,
    ) -> WireProgram {
        let mut program = valid_program();
        let argument_count = arguments.len();
        program.signatures = vec![actual, application];
        program.globals.push(crate::execution_schema::GlobalDecl {
            identity: symbol("callee"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: true,
            required_generation: None,
        });
        program.expressions.nodes = vec![ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
            signature: SignatureId(1),
            arguments,
        }];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        binding.binding.rhs = HeapRhs::Function {
            signature: SignatureId(1),
            parameters: (0..argument_count)
                .map(|index| ValueId(10 + index as u32))
                .collect(),
            captures: vec![],
            body: 0,
        };
        program
    }

    fn int_atom(value: i64) -> Atom {
        Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    }

    fn word_atom(value: u64) -> Atom {
        Atom::Scalar(ScalarLiteral::Word {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })
    }

    #[test]
    fn callable_application_checks_saturation_and_argument_prefixes() {
        let actual = Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        let saturated = callable_program(
            actual.clone(),
            Signature {
                arguments: actual.arguments.clone(),
                results: actual.results.clone(),
            },
            vec![int_atom(1), word_atom(2)],
        );
        validate_program(&saturated, &requirements(), DecodeLimits::default()).unwrap();

        let undersaturated = callable_program(
            actual.clone(),
            Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
            vec![int_atom(1)],
        );
        validate_program(&undersaturated, &requirements(), DecodeLimits::default()).unwrap();

        let oversaturated = callable_program(
            Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
            },
            vec![int_atom(1), word_atom(2)],
        );
        validate_program(&oversaturated, &requirements(), DecodeLimits::default()).unwrap();

        let prefix_mismatch = callable_program(
            actual.clone(),
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::Int(64)],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
            vec![int_atom(1), int_atom(2)],
        );
        assert_invalid_signature(prefix_mismatch);

        let saturated_result_mismatch = callable_program(
            actual,
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
                results: ResultContract::Returns(vec![RuntimeRep::Word(64)]),
            },
            vec![int_atom(1), word_atom(2)],
        );
        assert_invalid_signature(saturated_result_mismatch);
    }

    #[test]
    fn no_success_saturation_does_not_require_a_normal_result() {
        let program = callable_program(
            Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results: ResultContract::NoSuccess,
            },
            Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results: ResultContract::Returns(vec![RuntimeRep::Word(64)]),
            },
            vec![int_atom(1)],
        );
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn local_bottoming_entry_satisfies_a_normal_call_demand() {
        let mut program = valid_program();
        program.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::NoSuccess,
        });
        program.operations.push(OperationDecl {
            identity: crate::execution_schema::OperationIdentity::PrimOp("raise#".into()),
            signature: SignatureId(1),
        });
        program.expressions.nodes = vec![
            ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Local(ValueId(1))),
                signature: SignatureId(0),
                arguments: vec![],
            },
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![],
            },
        ];
        program.bindings.push(Group::NonRecursive(TopBinding {
            identity: symbol("bottom"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn no_success_demand_requires_evidence_and_partial_application_returns_a_function() {
        let mut normal = callable_program(
            Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
            },
            Signature {
                arguments: vec![],
                results: ResultContract::NoSuccess,
            },
            vec![],
        );
        assert_invalid_signature(normal.clone());
        normal.globals[0].entry_signature = None;
        assert_invalid_signature(normal);

        let actual = Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: ResultContract::NoSuccess,
        };
        assert_invalid_signature(callable_program(
            actual.clone(),
            Signature {
                arguments: vec![],
                results: ResultContract::NoSuccess,
            },
            vec![],
        ));
        let partial = callable_program(
            actual,
            Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
            vec![],
        );
        validate_program(&partial, &requirements(), DecodeLimits::default()).unwrap();

        assert_invalid_signature(callable_program(
            Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
            Signature {
                arguments: vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
                results: ResultContract::NoSuccess,
            },
            vec![int_atom(1), word_atom(2)],
        ));
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
            scrutinee_results: ResultContract::Returns(reps),
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
    fn empty_case_retains_known_reps_or_proves_bottom_without_a_binder_value() {
        let mut program = valid_program();
        program.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::NoSuccess,
        });
        program.operations.push(OperationDecl {
            identity: crate::execution_schema::OperationIdentity::PrimOp("raise#".into()),
            signature: SignatureId(1),
        });
        program.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![],
            },
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(1),
                scrutinee_results: ResultContract::NoSuccess,
                kind: CaseKind::Primitive(RuntimeRep::Int(64)),
                alternatives: vec![],
            },
        ];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = 1;
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();

        if let ExprFrame::Case { binder, .. } = &mut program.expressions.nodes[1] {
            *binder = ValueId(0);
        }
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition(_))
        ));
        if let ExprFrame::Case {
            binder,
            scrutinee_results,
            ..
        } = &mut program.expressions.nodes[1]
        {
            *binder = ValueId(1);
            *scrutinee_results = ResultContract::Returns(vec![RuntimeRep::Int(64)]);
        }
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
        program.expressions.nodes[0] = ExprFrame::Return(vec![integer(1)]);
        if let ExprFrame::Case {
            scrutinee_results, ..
        } = &mut program.expressions.nodes[1]
        {
            *scrutinee_results = ResultContract::NoSuccess;
        }
        assert_invalid_signature(program);
    }

    #[test]
    fn mixed_case_alternatives_merge_bottom_with_successful_representations() {
        let mut program = valid_program();
        program.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::NoSuccess,
        });
        program.operations.push(OperationDecl {
            identity: crate::execution_schema::OperationIdentity::PrimOp("raise#".into()),
            signature: SignatureId(1),
        });
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![integer(1)]),
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![],
            },
            ExprFrame::Return(vec![integer(42)]),
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(1),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
                kind: CaseKind::Primitive(RuntimeRep::Int(64)),
                alternatives: vec![
                    Alternative {
                        pattern: AlternativePattern::Literal(ScalarLiteral::Int {
                            bits: 64,
                            bytes: 1_i64.to_be_bytes().to_vec(),
                        }),
                        binders: vec![],
                        body: 1,
                    },
                    Alternative {
                        pattern: AlternativePattern::Default,
                        binders: vec![],
                        body: 2,
                    },
                ],
            },
        ];
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = 3;
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();

        program.signatures[0].results = ResultContract::NoSuccess;
        assert_invalid_signature(program.clone());
        program.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![],
        };
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
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
            host_id: crate::DataConId(2),
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
    fn reversed_top_level_dependencies_are_visible_before_rhs_walks() {
        let mut program = valid_program();
        program.expressions.nodes = vec![
            ExprFrame::Return(vec![integer(42)]),
            ExprFrame::Return(vec![integer(42)]),
        ];
        let top_a = TopBinding {
            identity: SymbolIdentity {
                module: "ModuleA".into(),
                ..symbol("a")
            },
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Local(ValueId(1))],
                    body: 0,
                },
            },
        };
        let top_b = TopBinding {
            identity: SymbolIdentity {
                module: "ModuleB".into(),
                ..symbol("b")
            },
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: 1,
                },
            },
        };
        program.bindings = vec![Group::NonRecursive(top_a), Group::NonRecursive(top_b)];
        validate_program(&program, &requirements(), DecodeLimits::default()).unwrap();
    }

    #[test]
    fn nonrecursive_top_level_self_reference_is_hidden_during_rhs_validation() {
        let mut program = valid_program();
        let Group::NonRecursive(binding) = &mut program.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { captures, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        captures.push(ValueRef::Local(binding.binding.id));
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidScope(_))
        ));
    }

    #[test]
    fn invalid_constructor_ids_are_reported_in_recursive_top_and_local_groups() {
        let mut top = valid_program();
        let Group::NonRecursive(binding) = top.bindings.pop().unwrap() else {
            unreachable!()
        };
        let mut binding = binding;
        binding.binding.rhs = HeapRhs::Constructor {
            constructor: ConstructorId(99),
            fields: vec![],
        };
        top.bindings.push(Group::Recursive(vec![binding]));
        top.expressions.nodes.clear();
        assert!(matches!(
            validate_program(&top, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(_))
        ));

        let mut local = valid_program();
        local.expressions.nodes = vec![
            ExprFrame::Return(vec![integer(42)]),
            ExprFrame::Let {
                bindings: Group::Recursive(vec![HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(99),
                        fields: vec![],
                    },
                }]),
                body: 0,
            },
        ];
        let Group::NonRecursive(binding) = &mut local.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut binding.binding.rhs else {
            unreachable!()
        };
        *body = 1;
        assert!(matches!(
            validate_program(&local, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(_))
        ));
    }

    #[test]
    fn top_metadata_errors_precede_earlier_rhs_errors() {
        let mut program = valid_program();
        let earlier = TopBinding {
            identity: symbol("earlier"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![ValueRef::Local(ValueId(99))],
                    body: 0,
                },
            },
        };
        let later = TopBinding {
            identity: symbol("later"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(99),
                    fields: vec![],
                },
            },
        };
        program.bindings = vec![Group::NonRecursive(earlier), Group::NonRecursive(later)];
        assert!(matches!(
            validate_program(&program, &requirements(), DecodeLimits::default()),
            Err(ParseError::InvalidReference(_))
        ));
    }

    #[test]
    fn rejects_nested_closure_with_missing_capture() {
        let mut program = valid_program();
        program.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        program.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
        program.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
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
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            },
            Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
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
            identity: super::super::OperationIdentity::PrimOp("op".into()),
            signature: SignatureId(0),
        });
        operation.signatures[0].arguments = vec![RuntimeRep::Int(64)];
        operation.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
    fn operation_identity_and_signature_form_a_unique_contract() {
        let identity = super::super::OperationIdentity::PrimOp("sameName".into());
        let mut duplicate = valid_program();
        duplicate.operations = vec![
            OperationDecl {
                identity: identity.clone(),
                signature: SignatureId(0),
            },
            OperationDecl {
                identity: identity.clone(),
                signature: SignatureId(0),
            },
        ];
        assert!(matches!(
            validate_program(&duplicate, &requirements(), DecodeLimits::default()),
            Err(ParseError::DuplicateDefinition(_))
        ));

        let mut distinct_signature = valid_program();
        distinct_signature.signatures.push(Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        distinct_signature.operations = vec![
            OperationDecl {
                identity: identity.clone(),
                signature: SignatureId(0),
            },
            OperationDecl {
                identity,
                signature: SignatureId(1),
            },
            OperationDecl {
                identity: super::super::OperationIdentity::Intrinsic {
                    symbol: "sameName".into(),
                    convention: super::super::ForeignConvention::CCall,
                },
                signature: SignatureId(0),
            },
        ];
        validate_program(
            &distinct_signature,
            &requirements(),
            DecodeLimits::default(),
        )
        .unwrap();
    }

    #[test]
    fn rejects_unnaturally_aligned_managed_layout() {
        let mut program = valid_program();
        program.constructors.push(ConstructorDecl {
            identity: symbol("C"),
            host_id: crate::DataConId(3),
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
    fn rejects_noncanonical_padding_between_storage_fields() {
        let mut program = valid_program();
        program.constructors.push(ConstructorDecl {
            identity: symbol("Padded"),
            host_id: crate::DataConId(4),
            family: symbol("T"),
            tag: 1,
            family_size: 1,
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![RuntimeRep::Int(8), RuntimeRep::Int(64), RuntimeRep::Int(8)],
            strict_fields: vec![true, true, true],
            layout: CheckedLayout {
                fields: vec![
                    FieldLayout {
                        rep: RuntimeRep::Int(8),
                        offset: 0,
                    },
                    FieldLayout {
                        rep: RuntimeRep::Int(64),
                        offset: 16,
                    },
                    FieldLayout {
                        rep: RuntimeRep::Int(8),
                        offset: 24,
                    },
                ],
                alignment: 8,
                payload_size: 32,
                root_mask: vec![false, false, false],
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
                        scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
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
