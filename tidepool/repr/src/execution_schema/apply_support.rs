//! Compact compiler-owned apply code without carrying its user's definition arena.

use super::*;
use recursion::MappableFrame;
use std::collections::BTreeSet;

const HELPERS: [&str; 3] = ["__resume", "__applyEntry", "__applyValue"];

fn refuse(detail: &str) -> ParseError {
    ParseError::InvalidReference(format!("apply support: {detail}"))
}

fn tops(groups: &[Group<TopBinding>]) -> impl Iterator<Item = &TopBinding> {
    groups.iter().flat_map(|group| match group {
        Group::NonRecursive(top) => std::slice::from_ref(top),
        Group::Recursive(tops) => tops.as_slice(),
    })
}

#[derive(Default)]
struct Tables {
    signatures: BTreeMap<SignatureId, SignatureId>,
    globals: BTreeMap<GlobalId, GlobalId>,
    constructors: BTreeMap<ConstructorId, ConstructorId>,
}

impl Tables {
    fn index<T: Copy + Ord>(
        table: &mut BTreeMap<T, T>,
        id: &mut T,
        collect: bool,
    ) -> Result<(), ParseError> {
        if collect {
            table.insert(*id, *id);
        } else {
            *id = *table
                .get(id)
                .ok_or_else(|| refuse("uncollected table reference"))?;
        }
        Ok(())
    }

    fn reference(&mut self, value: &mut ValueRef, collect: bool) -> Result<(), ParseError> {
        if let ValueRef::Global(id) = value {
            Self::index(&mut self.globals, id, collect)?;
        }
        Ok(())
    }

    fn atoms(&mut self, atoms: &mut [Atom], collect: bool) -> Result<(), ParseError> {
        for atom in atoms {
            match atom {
                Atom::Ref(value) => self.reference(value, collect)?,
                Atom::Void => {}
                Atom::Scalar(_) | Atom::Rubbish(_) => {
                    return Err(refuse("helper carries literal or rubbish data"))
                }
            }
        }
        Ok(())
    }

    fn heap(&mut self, rhs: &mut HeapRhs, collect: bool) -> Result<(), ParseError> {
        match rhs {
            HeapRhs::Function {
                signature,
                captures,
                ..
            }
            | HeapRhs::Thunk {
                signature,
                captures,
                ..
            } => {
                Self::index(&mut self.signatures, signature, collect)?;
                for capture in captures {
                    self.reference(capture, collect)?;
                }
            }
            HeapRhs::Constructor {
                constructor,
                fields,
            } => {
                Self::index(&mut self.constructors, constructor, collect)?;
                self.atoms(fields, collect)?;
            }
            HeapRhs::Bytes(_) => return Err(refuse("helper carries byte data")),
        }
        Ok(())
    }

    fn frame(&mut self, frame: &mut ExprFrame<usize>, collect: bool) -> Result<(), ParseError> {
        match frame {
            ExprFrame::Return(atoms)
            | ExprFrame::Jump {
                arguments: atoms, ..
            } => self.atoms(atoms, collect)?,
            ExprFrame::Enter { callee, signature } => {
                self.atoms(std::slice::from_mut(callee), collect)?;
                Self::index(&mut self.signatures, signature, collect)?;
            }
            ExprFrame::Call {
                callee,
                signature,
                arguments,
            } => {
                self.atoms(std::slice::from_mut(callee), collect)?;
                self.atoms(arguments, collect)?;
                Self::index(&mut self.signatures, signature, collect)?;
            }
            ExprFrame::Operation { .. } => {
                return Err(refuse("helper contains a machine operation"))
            }
            ExprFrame::Construct {
                constructor,
                fields,
            } => {
                Self::index(&mut self.constructors, constructor, collect)?;
                self.atoms(fields, collect)?;
            }
            ExprFrame::Case {
                kind, alternatives, ..
            } => {
                let CaseKind::Algebraic(family) = kind else {
                    return Err(refuse("helper case is not an issued effect family"));
                };
                let symbol = &family.symbol;
                if !((package(&symbol.unit, "freer-simple")
                    && symbol.module == "Control.Monad.Freer.Internal"
                    && symbol.occurrence == "Eff")
                    || (package(&symbol.unit, "tidepool-resume")
                        && symbol.module == "Tidepool.Internal.Resume"
                        && symbol.occurrence == "Settled"))
                    || symbol.namespace != "type"
                    || symbol.record_parent.is_some()
                {
                    return Err(refuse("helper case carries a user family"));
                }
                for alternative in alternatives {
                    match &mut alternative.pattern {
                        AlternativePattern::Constructor(id) => {
                            Self::index(&mut self.constructors, id, collect)?
                        }
                        AlternativePattern::Default => {}
                        AlternativePattern::Literal(_) => {
                            return Err(refuse("helper case carries a literal"))
                        }
                    }
                }
            }
            ExprFrame::Let { bindings, .. } => match bindings {
                Group::NonRecursive(binding) => self.heap(&mut binding.rhs, collect)?,
                Group::Recursive(bindings) => {
                    for binding in bindings {
                        self.heap(&mut binding.rhs, collect)?;
                    }
                }
            },
            ExprFrame::LetJoins { bindings, .. } => {
                let mut join = |binding: &mut JoinBinding| {
                    Self::index(&mut self.signatures, &mut binding.signature, collect)
                };
                match bindings {
                    Group::NonRecursive(binding) => join(binding)?,
                    Group::Recursive(bindings) => {
                        for binding in bindings {
                            join(binding)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn package(unit: &str, name: &str) -> bool {
    unit == name
        || unit
            .strip_prefix(name)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

fn allowed_constructor(declaration: &ConstructorDecl, resume_unit: &str) -> bool {
    let symbol = &declaration.identity.symbol;
    let family = &declaration.identity.family;
    if symbol.namespace != "constructor" || symbol.record_parent.is_some() {
        return false;
    }
    let wired_int = symbol.unit == "ghc-prim"
        && symbol.module == "GHC.Types"
        && symbol.occurrence == "I#"
        && family.symbol.unit == symbol.unit
        && family.symbol.module == symbol.module
        && family.symbol.namespace == "type"
        && family.symbol.occurrence == "Int"
        && family.symbol.record_parent.is_none()
        && family.version == TypeVersion::WiredIn;
    let settled = symbol.unit == resume_unit
        && symbol.module == "Tidepool.Internal.Resume"
        && matches!(symbol.occurrence.as_str(), "Done" | "Suspended")
        && family.symbol.unit == resume_unit
        && family.symbol.module == symbol.module
        && family.symbol.namespace == "type"
        && family.symbol.occurrence == "Settled"
        && family.symbol.record_parent.is_none();
    wired_int || settled
}

pub(super) fn extract(program: &PreparedProgram) -> Result<Option<PreparedProgram>, ParseError> {
    let source = program.wire.as_ref();
    let owner = &tops(&source.bindings)
        .find(|top| top.binding.id == source.entry)
        .ok_or_else(|| refuse("entry has no defining top"))?
        .identity;
    let mut helpers = Vec::new();
    for name in HELPERS {
        let mut found = tops(&source.bindings).filter(|top| {
            top.identity.unit == owner.unit
                && top.identity.module == owner.module
                && top.identity.occurrence == name
        });
        let Some(top) = found.next() else {
            return Ok(None);
        };
        if found.next().is_some()
            || top.identity.namespace != "value"
            || top.identity.record_parent.is_some()
        {
            return Err(refuse("helper identity is not unique and ordinary"));
        }
        helpers.push(top.clone());
    }

    let limits = DecodeLimits::default();
    let mut budget = OperationBudget::new(limits.max_work);
    let mut tables = Tables::default();
    let mut pending = Vec::new();
    for top in &mut helpers {
        budget.charge_symbol_copy(&top.identity)?;
        let HeapRhs::Function {
            signature,
            parameters,
            captures,
            body,
        } = &top.binding.rhs
        else {
            return Err(refuse("helper top is not a function"));
        };
        let expected = if top.identity.occurrence == "__applyEntry" {
            vec![
                RuntimeRep::LiftedRef,
                RuntimeRep::Int(source.envelope.target.word_width),
            ]
        } else {
            vec![RuntimeRep::LiftedRef; 2]
        };
        let contract = &source.signatures[signature.0 as usize];
        if parameters.len() != 2
            || !captures.is_empty()
            || contract.arguments != expected
            || contract.results != ResultContract::Returns(vec![RuntimeRep::LiftedRef])
        {
            return Err(refuse("helper top has an unexpected call contract"));
        }
        pending.push(*body);
        tables.heap(&mut top.binding.rhs, true)?;
    }
    let mut nodes = BTreeMap::new();
    while let Some(index) = pending.pop() {
        if nodes.contains_key(&index) {
            continue;
        }
        budget.reserve::<ExprFrame<usize>>(1)?;
        let mut frame = source.expressions.nodes[index].clone();
        tables.frame(&mut frame, true)?;
        let frame = ExprFrame::<recursion::PartiallyApplied>::map_frame(frame, |child| {
            pending.push(child);
            child
        });
        nodes.insert(index, frame);
    }

    let mut resume_unit: Option<&str> = None;
    for id in tables.globals.keys() {
        let global = &source.globals[id.0 as usize];
        let symbol = &global.identity;
        if !package(&symbol.unit, "tidepool-resume")
            || symbol.module != "Tidepool.Internal.Resume"
            || symbol.namespace != "value"
            || symbol.record_parent.is_some()
            || !matches!(symbol.occurrence.as_str(), "settle" | "resumeLifted")
            || global.required_generation.is_some()
            || global.rep != RuntimeRep::LiftedRef
        {
            return Err(refuse("helper imports a user or non-resume value"));
        }
        if resume_unit
            .replace(&symbol.unit)
            .is_some_and(|unit| unit != symbol.unit)
        {
            return Err(refuse("helper imports competing resume packages"));
        }
        if let Some(signature) = global.entry_signature {
            tables.signatures.insert(signature, signature);
        }
    }
    let resume_unit =
        resume_unit.ok_or_else(|| refuse("helpers lack their resume package references"))?;
    // Settled declarations, when emitted, also serve the runtime's settled-layer
    // observer. Missing declarations stay missing; the linked package owns them.
    for (index, constructor) in source.constructors.iter().enumerate() {
        if constructor.identity.symbol.unit == resume_unit
            && constructor.identity.family.symbol.occurrence == "Settled"
        {
            tables
                .constructors
                .insert(ConstructorId(index as u32), ConstructorId(index as u32));
        }
    }
    for id in tables.constructors.keys() {
        if !allowed_constructor(&source.constructors[id.0 as usize], resume_unit) {
            return Err(refuse("helper references a user constructor"));
        }
    }
    for (index, new) in tables.signatures.values_mut().enumerate() {
        *new = SignatureId(index as u32);
    }
    for (index, new) in tables.globals.values_mut().enumerate() {
        *new = GlobalId(index as u32);
    }
    for (index, new) in tables.constructors.values_mut().enumerate() {
        *new = ConstructorId(index as u32);
    }
    let expression_ids = nodes
        .keys()
        .enumerate()
        .map(|(new, old)| (*old, new))
        .collect::<BTreeMap<_, _>>();
    let mut expressions = Vec::with_capacity(nodes.len());
    for (_, mut frame) in nodes {
        tables.frame(&mut frame, false)?;
        expressions.push(ExprFrame::<recursion::PartiallyApplied>::map_frame(
            frame,
            |old| expression_ids[&old],
        ));
    }
    for top in &mut helpers {
        tables.heap(&mut top.binding.rhs, false)?;
        top.binding.rhs = top.binding.rhs.clone().map_body(|old| expression_ids[&old]);
    }
    let globals = tables
        .globals
        .keys()
        .map(|id| {
            let mut global = source.globals[id.0 as usize].clone();
            global.entry_signature = global.entry_signature.map(|id| tables.signatures[&id]);
            global
        })
        .collect();
    let wire = WireProgram {
        envelope: source.envelope.clone(),
        signatures: tables
            .signatures
            .keys()
            .map(|id| source.signatures[id.0 as usize].clone())
            .collect(),
        globals,
        constructors: tables
            .constructors
            .keys()
            .map(|id| source.constructors[id.0 as usize].clone())
            .collect(),
        operations: vec![],
        expressions: Expr { nodes: expressions },
        entry: helpers[0].binding.id,
        bindings: helpers.into_iter().map(Group::NonRecursive).collect(),
        types: Arc::default(),
        sites: vec![],
        constructor_replies: vec![],
        json_layout: None,
    };
    let requirements = ProgramRequirements {
        schema_version: source.envelope.schema_version,
        projection_profile: source.envelope.projection_profile.clone(),
        toolchain: source.envelope.toolchain.clone(),
        execution_abi_version: source.envelope.execution_abi_version,
        target: source.envelope.target.clone(),
    };
    validation::validate_program_with_budget(&wire, &requirements, limits, &mut budget)?;
    Ok(Some(super::prepared_from_validated(wire)))
}

#[cfg(test)]
mod tests;
