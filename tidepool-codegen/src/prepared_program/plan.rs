//! Checked binder and closure layout facts used by every emitter consumer.

use super::CompileError;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tidepool_heap::execution_descriptor::{EntryMetadata, ObjectDescriptor, ObjectKind};
use tidepool_repr::execution_schema::{
    AlternativePattern, Atom, CaseKind, ExprFrame, Group, HeapBinding, HeapRhs, PreparedProgram,
    RuntimeRep, ScalarLiteral, Signature, StorageLayout, ValueId, ValueRef,
};

pub(super) struct FunctionPlan<'a> {
    pub signature: &'a Signature,
    pub parameters: &'a [ValueId],
    pub captures: &'a [ValueRef],
    pub body: usize,
    pub descriptor: Arc<ObjectDescriptor>,
}

/// A thunk has the same captured environment layout as a function, but its
/// generated body is entered by the lazy state machine and therefore has a
/// closed `[] -> [LiftedRef]` semantic signature.
pub(super) struct ThunkPlan<'a> {
    pub signature: &'a Signature,
    pub captures: &'a [ValueRef],
    pub body: usize,
    pub policy: tidepool_repr::execution_schema::UpdatePolicy,
    pub descriptor: Arc<ObjectDescriptor>,
}

/// Invocation-owned top allocation recipe. The checked binding is cloned so
/// run initialization can reserve the complete group after static addresses
/// exist, without retaining the input artifact behind the compiled program.
pub(crate) struct HeapTopSpec {
    pub id: ValueId,
    pub descriptor: Arc<ObjectDescriptor>,
    pub binding: HeapBinding,
    pub reps: Vec<RuntimeRep>,
}

pub(super) struct ProgramPlan<'a> {
    pub program: &'a PreparedProgram,
    pub functions: BTreeMap<ValueId, FunctionPlan<'a>>,
    pub thunks: BTreeMap<ValueId, ThunkPlan<'a>>,
    pub top_bindings: BTreeMap<ValueId, &'a HeapBinding>,
    /// Logical representations, including Void. ValueIds are globally unique
    /// after validation, so no lexical search or scope cloning is necessary.
    pub values: BTreeMap<ValueId, RuntimeRep>,
    pub constructors: Vec<Arc<ObjectDescriptor>>,
    /// Compact slots, not ValueId-indexed allocation controlled by wire IDs.
    pub top_slots: BTreeMap<ValueId, usize>,
    /// Pinned literal payloads; emitters never embed a borrowed artifact buffer.
    pub bytes: BTreeMap<Vec<u8>, Arc<[u8]>>,
    pub heap_tops: BTreeSet<ValueId>,
    pub heap_top_specs: Vec<HeapTopSpec>,
    /// Flattened pending argument layouts owned by the application emitter.
    /// Keys use the original function and logical (Void-inclusive) prefix
    /// length, so dispatch never infers storage slots from physical arity.
    pub pap_layouts: BTreeMap<(ValueId, usize), super::apply::PapLayout>,
}

impl<'a> ProgramPlan<'a> {
    pub fn new(program: &'a PreparedProgram) -> Result<Self, CompileError> {
        // wave4:LAYOUT_PLAN — collect top/local RHS types, function and join
        // parameter reps, case binder/alternative reps from checked signatures
        // and ConstructorDecl. Multi-component case binder is non-value Void.
        // Use authoritative captures, not a second free-variable analysis.
        // Only after all IDs have reps, construct pinned function layouts from
        // capture reps and constructor layouts from their owning declarations.
        let mut values = BTreeMap::new();
        let mut top_bindings = BTreeMap::new();
        let mut top_slots = BTreeMap::new();
        let mut bytes = BTreeMap::new();

        for group in program.bindings() {
            for top in group_items(group) {
                top_bindings.insert(top.binding.id, &top.binding);
                let slot = top_slots.len();
                top_slots.insert(top.binding.id, slot);
                binding_rep(program, &top.binding, &mut values);
                collect_binding_literals(&top.binding, &mut bytes);
            }
        }

        for frame in &program.expressions().nodes {
            match frame {
                ExprFrame::Return(atoms)
                | ExprFrame::Jump {
                    arguments: atoms, ..
                } => {
                    collect_atoms(atoms, &mut bytes);
                }
                ExprFrame::Enter { callee, .. } => collect_atom(callee, &mut bytes),
                ExprFrame::Call {
                    callee, arguments, ..
                } => {
                    collect_atom(callee, &mut bytes);
                    collect_atoms(arguments, &mut bytes);
                }
                ExprFrame::Operation { arguments, .. }
                | ExprFrame::Construct {
                    fields: arguments, ..
                } => collect_atoms(arguments, &mut bytes),
                ExprFrame::Case {
                    binder,
                    scrutinee_reps,
                    kind,
                    alternatives,
                    ..
                } => {
                    let binder_rep = if *kind == CaseKind::MultiValue {
                        RuntimeRep::Void
                    } else if let [rep] = scrutinee_reps.as_slice() {
                        *rep
                    } else {
                        RuntimeRep::Void
                    };
                    values.insert(*binder, binder_rep);
                    for alternative in alternatives {
                        if let AlternativePattern::Literal(literal) = &alternative.pattern {
                            collect_literal(literal, &mut bytes);
                        }
                        let reps: &[RuntimeRep] = match (&alternative.pattern, kind) {
                            (_, CaseKind::MultiValue) => scrutinee_reps.as_slice(),
                            (AlternativePattern::Constructor(id), _) => {
                                &program.constructors()[id.0 as usize].field_reps
                            }
                            _ => &[],
                        };
                        for (id, rep) in alternative.binders.iter().zip(reps) {
                            values.insert(*id, *rep);
                        }
                    }
                }
                ExprFrame::Let { bindings, .. } => {
                    for binding in group_items(bindings) {
                        binding_rep(program, binding, &mut values);
                        collect_binding_literals(binding, &mut bytes);
                    }
                }
                ExprFrame::LetJoins { bindings, .. } => {
                    for binding in group_items(bindings) {
                        let signature = signature(program, binding.signature);
                        for (id, rep) in binding.parameters.iter().zip(&signature.arguments) {
                            values.insert(*id, *rep);
                        }
                    }
                }
            }
        }

        let target = &program.envelope().target;
        let mut constructors = Vec::with_capacity(program.constructors().len());
        for declaration in program.constructors() {
            let layout = StorageLayout::for_reps(target, &declaration.field_reps)?;
            constructors.push(Arc::new(ObjectDescriptor::constructor(
                declaration.tag,
                layout,
                None,
            )?));
        }

        let bindings = all_bindings(program);
        for binding in &bindings {
            if let HeapRhs::Function {
                signature: signature_id,
                parameters,
                ..
            } = &binding.rhs
            {
                let signature = signature(program, *signature_id);
                for (id, rep) in parameters.iter().zip(&signature.arguments) {
                    values.insert(*id, *rep);
                }
            }
        }

        let mut functions = BTreeMap::new();
        let mut thunks = BTreeMap::new();
        for binding in bindings {
            match &binding.rhs {
                HeapRhs::Function {
                    signature: signature_id,
                    parameters,
                    captures,
                    body,
                } => {
                    let signature = signature(program, *signature_id);
                    let capture_reps = captures
                        .iter()
                        .map(|capture| value_ref_rep(&values, capture))
                        .collect::<Result<Vec<_>, _>>()?;
                    let layout = StorageLayout::for_reps(target, &capture_reps)?;
                    let descriptor = Arc::new(ObjectDescriptor::new(
                        ObjectKind::Function,
                        layout,
                        Some(EntryMetadata::new(
                            signature.clone(),
                            u64::from(binding.id.0),
                        )),
                    )?);
                    functions.insert(
                        binding.id,
                        FunctionPlan {
                            signature,
                            parameters,
                            captures,
                            body: *body,
                            descriptor,
                        },
                    );
                }
                HeapRhs::Thunk {
                    signature: signature_id,
                    update,
                    captures,
                    body,
                } => {
                    let signature = signature(program, *signature_id);
                    if !signature.arguments.is_empty()
                        || signature.results.as_slice() != [RuntimeRep::LiftedRef]
                    {
                        return Err(CompileError::Unsupported(
                            super::Unsupported::ThunkSignature(binding.id),
                        ));
                    }
                    let capture_reps = captures
                        .iter()
                        .map(|capture| value_ref_rep(&values, capture))
                        .collect::<Result<Vec<_>, _>>()?;
                    let layout = StorageLayout::for_reps(target, &capture_reps)?;
                    let descriptor = Arc::new(ObjectDescriptor::new(
                        ObjectKind::Thunk,
                        layout,
                        Some(EntryMetadata::new(
                            Signature {
                                arguments: Vec::new(),
                                results: vec![RuntimeRep::LiftedRef],
                            },
                            u64::from(binding.id.0),
                        )),
                    )?);
                    thunks.insert(
                        binding.id,
                        ThunkPlan {
                            signature,
                            captures,
                            body: *body,
                            policy: *update,
                            descriptor,
                        },
                    );
                }
                _ => {}
            }
        }

        let heap_tops = super::image::heap_top_partition(&top_bindings);
        let pap_layouts = super::apply::layouts(
            target,
            functions
                .iter()
                .map(|(&id, function)| (id, function.signature)),
        )?;
        let mut heap_top_specs = Vec::new();
        for (&id, binding) in &top_bindings {
            if !heap_tops.contains(&id) {
                continue;
            }
            let descriptor = match &binding.rhs {
                HeapRhs::Function { .. } => functions
                    .get(&id)
                    .map(|function| Arc::clone(&function.descriptor)),
                HeapRhs::Thunk { .. } => thunks.get(&id).map(|thunk| Arc::clone(&thunk.descriptor)),
                HeapRhs::Constructor { constructor, .. } => {
                    constructors.get(constructor.0 as usize).cloned()
                }
                HeapRhs::Bytes(_) => None,
            }
            .ok_or(CompileError::MissingRepresentation(id))?;
            let reps = match &binding.rhs {
                HeapRhs::Constructor { constructor, .. } => program.constructors()
                    [constructor.0 as usize]
                    .field_reps
                    .clone(),
                HeapRhs::Function { captures, .. } | HeapRhs::Thunk { captures, .. } => captures
                    .iter()
                    .map(|capture| value_ref_rep(&values, capture))
                    .collect::<Result<Vec<_>, _>>()?,
                HeapRhs::Bytes(_) => Vec::new(),
            };
            heap_top_specs.push(HeapTopSpec {
                id,
                descriptor,
                binding: (*binding).clone(),
                reps,
            });
        }

        Ok(Self {
            program,
            functions,
            thunks,
            top_bindings,
            values,
            constructors,
            top_slots,
            bytes,
            heap_tops,
            heap_top_specs,
            pap_layouts,
        })
    }
}

fn group_items<T>(group: &Group<T>) -> &[T] {
    match group {
        Group::NonRecursive(item) => std::slice::from_ref(item),
        Group::Recursive(items) => items,
    }
}

fn all_bindings(program: &PreparedProgram) -> Vec<&HeapBinding> {
    let mut bindings = Vec::new();
    for group in program.bindings() {
        bindings.extend(group_items(group).iter().map(|top| &top.binding));
    }
    for frame in &program.expressions().nodes {
        if let ExprFrame::Let {
            bindings: group, ..
        } = frame
        {
            bindings.extend(group_items(group));
        }
    }
    bindings
}

fn signature(
    program: &PreparedProgram,
    id: tidepool_repr::execution_schema::SignatureId,
) -> &Signature {
    &program.signatures()[id.0 as usize]
}

fn binding_rep(
    program: &PreparedProgram,
    binding: &HeapBinding,
    values: &mut BTreeMap<ValueId, RuntimeRep>,
) {
    let rep = match &binding.rhs {
        HeapRhs::Bytes(_) => RuntimeRep::Address,
        HeapRhs::Function { .. } | HeapRhs::Thunk { .. } => RuntimeRep::LiftedRef,
        HeapRhs::Constructor { constructor, .. } => {
            program.constructors()[constructor.0 as usize].result_rep
        }
    };
    values.insert(binding.id, rep);
}

fn value_ref_rep(
    values: &BTreeMap<ValueId, RuntimeRep>,
    value: &ValueRef,
) -> Result<RuntimeRep, CompileError> {
    match value {
        ValueRef::Local(id) => values
            .get(id)
            .copied()
            .ok_or(CompileError::MissingRepresentation(*id)),
        ValueRef::Global(id) => Err(CompileError::Unsupported(super::Unsupported::Global(*id))),
    }
}

fn collect_binding_literals(binding: &HeapBinding, bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    match &binding.rhs {
        HeapRhs::Bytes(value) => pin_bytes(value, bytes),
        HeapRhs::Constructor { fields, .. } => collect_atoms(fields, bytes),
        HeapRhs::Function { .. } | HeapRhs::Thunk { .. } => {}
    }
}

fn collect_atoms(atoms: &[Atom], bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    for atom in atoms {
        collect_atom(atom, bytes);
    }
}

fn collect_atom(atom: &Atom, bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    if let Atom::Scalar(literal) = atom {
        collect_literal(literal, bytes);
    }
}

fn collect_literal(literal: &ScalarLiteral, bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    if let ScalarLiteral::Bytes(value) = literal {
        pin_bytes(value, bytes);
    }
}

fn pin_bytes(value: &[u8], bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    bytes
        .entry(value.to_vec())
        .or_insert_with(|| Arc::<[u8]>::from(value.to_vec()));
}
