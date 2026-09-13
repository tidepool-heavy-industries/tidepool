use super::{plan::ProgramPlan, CompileError, Unsupported};
use std::collections::{BTreeMap, BTreeSet};
use std::ptr;
use std::sync::Arc;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::static_region::{StaticImage, StaticImageError, StaticRelocation};
use tidepool_repr::execution_schema::{
    Atom, HeapBinding, HeapRhs, RuntimeRep, ScalarLiteral, ValueId, ValueRef,
};

/// Reverse managed-edge closure of CAFs. A constructor or function which
/// contains a pointer to a mutable top must move with it; static code referring
/// to a top through VMContext is not a heap edge. The remaining graph is closed
/// and immutable, so the collector may continue skipping its fields.
pub(super) fn heap_top_partition(tops: &BTreeMap<ValueId, &HeapBinding>) -> BTreeSet<ValueId> {
    let mut reverse = BTreeMap::<ValueId, Vec<ValueId>>::new();
    let mut heap = BTreeSet::new();
    let mut pending = Vec::new();
    for (&owner, binding) in tops {
        if matches!(binding.rhs, HeapRhs::Thunk { .. }) {
            heap.insert(owner);
            pending.push(owner);
        }
        let mut edge = |reference: &ValueRef| {
            if let ValueRef::Local(target) = reference {
                if tops.contains_key(target) {
                    reverse.entry(*target).or_default().push(owner);
                }
            }
        };
        match &binding.rhs {
            HeapRhs::Function { captures, .. } | HeapRhs::Thunk { captures, .. } => {
                for reference in captures {
                    edge(reference);
                }
            }
            HeapRhs::Constructor { fields, .. } => {
                for atom in fields {
                    if let Atom::Ref(reference) = atom {
                        edge(reference);
                    }
                }
            }
            HeapRhs::Bytes(_) => {}
        }
    }
    while let Some(target) = pending.pop() {
        if let Some(owners) = reverse.get(&target) {
            for &owner in owners {
                if heap.insert(owner) {
                    pending.push(owner);
                }
            }
        }
    }
    heap
}

#[cfg(test)]
mod partition_tests {
    use super::*;
    use tidepool_repr::execution_schema::{ConstructorId, SignatureId, UpdatePolicy};

    #[test]
    fn w5_a1_caf_partition_moves_reverse_closure_only() {
        let caf = HeapBinding {
            id: ValueId(0),
            rhs: HeapRhs::Thunk {
                signature: SignatureId(0),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body: 0,
            },
        };
        let container = HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: vec![Atom::Ref(ValueRef::Local(caf.id))],
            },
        };
        let closure = HeapBinding {
            id: ValueId(2),
            rhs: HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![],
                captures: vec![ValueRef::Local(container.id)],
                body: 0,
            },
        };
        let independent = HeapBinding {
            id: ValueId(3),
            rhs: HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: vec![],
            },
        };
        let tops = [&caf, &container, &closure, &independent]
            .into_iter()
            .map(|binding| (binding.id, binding))
            .collect();
        assert_eq!(
            heap_top_partition(&tops),
            BTreeSet::from([caf.id, container.id, closure.id])
        );
    }
}

/// Reserve every top object before initializing any managed edge. Function
/// captures and constructor fields use the same descriptor logical layout as
/// generated allocation. Raw byte addresses point into ProgramPlan's pinned
/// bytes, never the input artifact. Bytes tops occupy top-table slots but are
/// not fake objects in the descriptor image.
pub(super) fn build_static_image(plan: &ProgramPlan<'_>) -> Result<StaticImage, CompileError> {
    let mut ordered_tops: Vec<_> = plan
        .top_slots
        .iter()
        .map(|(id, slot)| (*slot, *id))
        .collect();
    ordered_tops.sort_unstable_by_key(|(slot, _)| *slot);

    let mut objects = Vec::new();
    objects
        .try_reserve_exact(plan.top_slots.len())
        .map_err(|_| CompileError::Static(StaticImageError::Allocation))?;
    let mut top_objects = BTreeMap::new();
    let mut total_bytes = 0usize;
    for (_, id) in ordered_tops {
        if plan.heap_tops.contains(&id) {
            continue;
        }
        let binding = plan
            .top_bindings
            .get(&id)
            .copied()
            .ok_or(CompileError::MissingRepresentation(id))?;
        let Some(descriptor) = top_descriptor(plan, id, binding) else {
            continue;
        };
        let offset = total_bytes;
        total_bytes = total_bytes
            .checked_add(descriptor.allocation_extent() as usize)
            .ok_or(CompileError::Static(StaticImageError::Allocation))?;
        top_objects.insert(id, (offset, Arc::clone(&descriptor)));
        objects.push((id, binding, descriptor, offset));
    }

    if !total_bytes.is_multiple_of(std::mem::size_of::<u64>()) {
        return Err(CompileError::Static(StaticImageError::Allocation));
    }
    let word_count = total_bytes / std::mem::size_of::<u64>();
    let mut words = Vec::new();
    words
        .try_reserve_exact(word_count)
        .map_err(|_| CompileError::Static(StaticImageError::Allocation))?;
    words.resize(word_count, 0);

    let mut relocations = Vec::new();
    let relocation_capacity = objects
        .iter()
        .try_fold(0usize, |total, (_, _, descriptor, _)| {
            total
                .checked_add(descriptor.trace_offsets().len())
                .ok_or(CompileError::Static(StaticImageError::Allocation))
        })?;
    relocations
        .try_reserve_exact(relocation_capacity)
        .map_err(|_| CompileError::Static(StaticImageError::Allocation))?;
    for (_, binding, descriptor, offset) in &objects {
        unsafe {
            descriptor.initialize_header(words.as_mut_ptr().cast::<u8>().add(*offset));
        }
        match &binding.rhs {
            HeapRhs::Constructor {
                constructor,
                fields,
            } => {
                let reps = &plan.program.constructors()[constructor.0 as usize].field_reps;
                initialize_atoms(
                    plan,
                    &top_objects,
                    &mut words,
                    *offset,
                    descriptor,
                    fields,
                    reps,
                    &mut relocations,
                )?;
            }
            HeapRhs::Function { .. } => {
                let function = plan
                    .functions
                    .get(&binding.id)
                    .ok_or(CompileError::MissingRepresentation(binding.id))?;
                initialize_captures(
                    plan,
                    &top_objects,
                    &mut words,
                    *offset,
                    descriptor,
                    function.captures,
                    &mut relocations,
                )?;
            }
            HeapRhs::Bytes(_) | HeapRhs::Thunk { .. } => unreachable!(),
        }
    }

    let mut entries = BTreeMap::new();
    for (id, (offset, _)) in top_objects {
        entries.insert(id, offset);
    }

    let mut descriptors = Vec::new();
    let descriptor_capacity = plan
        .constructors
        .len()
        .checked_add(objects.len())
        .ok_or(CompileError::Static(StaticImageError::Allocation))?;
    descriptors
        .try_reserve(descriptor_capacity)
        .map_err(|_| CompileError::Static(StaticImageError::Allocation))?;
    descriptors.extend(plan.constructors.iter().cloned());
    descriptors.extend(
        objects
            .iter()
            .map(|(_, _, descriptor, _)| Arc::clone(descriptor)),
    );
    Ok(StaticImage::new(words, relocations, entries, descriptors)?)
}

fn top_descriptor(
    plan: &ProgramPlan<'_>,
    id: ValueId,
    binding: &HeapBinding,
) -> Option<Arc<ObjectDescriptor>> {
    match &binding.rhs {
        HeapRhs::Constructor { constructor, .. } => {
            plan.constructors.get(constructor.0 as usize).cloned()
        }
        HeapRhs::Function { .. } => plan
            .functions
            .get(&id)
            .map(|function| Arc::clone(&function.descriptor)),
        HeapRhs::Bytes(_) | HeapRhs::Thunk { .. } => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "static atom initialization independently borrows the plan, object index, image, descriptor, field values, representations, and relocation custody"
)]
fn initialize_atoms(
    plan: &ProgramPlan<'_>,
    top_objects: &BTreeMap<ValueId, (usize, Arc<ObjectDescriptor>)>,
    words: &mut [u64],
    object_offset: usize,
    descriptor: &ObjectDescriptor,
    atoms: &[Atom],
    reps: &[RuntimeRep],
    relocations: &mut Vec<StaticRelocation>,
) -> Result<(), CompileError> {
    for (logical, (atom, rep)) in atoms.iter().zip(reps).enumerate() {
        let Some(stored) = descriptor
            .payload()
            .logical_to_stored()
            .get(logical)
            .and_then(|slot| *slot)
        else {
            continue;
        };
        let field = &descriptor.payload().fields()[stored as usize];
        let field_offset = object_offset
            .checked_add(descriptor.payload_base() as usize)
            .and_then(|offset| offset.checked_add(field.offset() as usize))
            .ok_or(CompileError::Static(StaticImageError::Allocation))?;
        initialize_atom(
            plan,
            top_objects,
            words,
            field_offset,
            field.size() as usize,
            atom,
            *rep,
            relocations,
        )?;
    }
    Ok(())
}

fn initialize_captures(
    plan: &ProgramPlan<'_>,
    top_objects: &BTreeMap<ValueId, (usize, Arc<ObjectDescriptor>)>,
    words: &mut [u64],
    object_offset: usize,
    descriptor: &ObjectDescriptor,
    captures: &[ValueRef],
    relocations: &mut Vec<StaticRelocation>,
) -> Result<(), CompileError> {
    for (logical, capture) in captures.iter().enumerate() {
        let Some(stored) = descriptor
            .payload()
            .logical_to_stored()
            .get(logical)
            .and_then(|slot| *slot)
        else {
            continue;
        };
        let field = &descriptor.payload().fields()[stored as usize];
        let field_offset = object_offset
            .checked_add(descriptor.payload_base() as usize)
            .and_then(|offset| offset.checked_add(field.offset() as usize))
            .ok_or(CompileError::Static(StaticImageError::Allocation))?;
        initialize_capture(
            plan,
            top_objects,
            words,
            field_offset,
            field.size() as usize,
            capture,
            field.rep(),
            relocations,
        )?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "one static atom independently needs resolution state, image storage, field bounds, representation, value, and relocation custody"
)]
fn initialize_atom(
    plan: &ProgramPlan<'_>,
    top_objects: &BTreeMap<ValueId, (usize, Arc<ObjectDescriptor>)>,
    words: &mut [u64],
    field_offset: usize,
    field_size: usize,
    atom: &Atom,
    rep: RuntimeRep,
    relocations: &mut Vec<StaticRelocation>,
) -> Result<(), CompileError> {
    match (rep, atom) {
        (RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef, Atom::Ref(value)) => {
            add_relocation(plan, top_objects, value, field_offset, relocations)
        }
        (RuntimeRep::Address, Atom::Ref(value)) => {
            let address = resolve_address(plan, top_objects, value)?;
            write_pointer(words, field_offset, field_size, address)
        }
        (RuntimeRep::Address, Atom::Scalar(ScalarLiteral::Bytes(value))) => {
            let address = plan
                .bytes
                .get(value)
                .ok_or(CompileError::MissingRepresentation(plan.program.entry()))?
                .as_ptr() as usize;
            write_pointer(words, field_offset, field_size, address)
        }
        (RuntimeRep::Address, Atom::Scalar(ScalarLiteral::NullAddress)) => {
            write_pointer(words, field_offset, field_size, 0)
        }
        (
            RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_),
            Atom::Scalar(literal),
        ) => write_scalar(plan, words, field_offset, field_size, literal),
        (RuntimeRep::Void, Atom::Void) => Ok(()),
        _ => Err(invalid_static_value(plan)),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one static capture independently needs resolution state, image storage, field bounds, representation, value, and relocation custody"
)]
fn initialize_capture(
    plan: &ProgramPlan<'_>,
    top_objects: &BTreeMap<ValueId, (usize, Arc<ObjectDescriptor>)>,
    words: &mut [u64],
    field_offset: usize,
    field_size: usize,
    capture: &ValueRef,
    rep: RuntimeRep,
    relocations: &mut Vec<StaticRelocation>,
) -> Result<(), CompileError> {
    match rep {
        RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef => {
            add_relocation(plan, top_objects, capture, field_offset, relocations)
        }
        RuntimeRep::Address => {
            let address = resolve_address(plan, top_objects, capture)?;
            write_pointer(words, field_offset, field_size, address)
        }
        RuntimeRep::Void => Ok(()),
        RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_) => {
            Err(invalid_static_value(plan))
        }
    }
}

fn add_relocation(
    plan: &ProgramPlan<'_>,
    top_objects: &BTreeMap<ValueId, (usize, Arc<ObjectDescriptor>)>,
    value: &ValueRef,
    slot_offset: usize,
    relocations: &mut Vec<StaticRelocation>,
) -> Result<(), CompileError> {
    let ValueRef::Local(id) = value else {
        return Err(CompileError::Unsupported(Unsupported::Global(
            match value {
                ValueRef::Global(id) => *id,
                ValueRef::Local(_) => unreachable!(),
            },
        )));
    };
    let Some((target_offset, target_descriptor)) = top_objects.get(id) else {
        if plan.heap_tops.contains(id) {
            return Err(CompileError::Unsupported(Unsupported::StaticHeapEdge(*id)));
        }
        return Err(CompileError::MissingRepresentation(*id));
    };
    relocations.push(StaticRelocation {
        slot_offset,
        target_offset: *target_offset,
        tag: target_descriptor.tag(),
    });
    Ok(())
}

fn resolve_address(
    plan: &ProgramPlan<'_>,
    top_objects: &BTreeMap<ValueId, (usize, Arc<ObjectDescriptor>)>,
    value: &ValueRef,
) -> Result<usize, CompileError> {
    let ValueRef::Local(id) = value else {
        return Err(CompileError::Unsupported(Unsupported::Global(
            match value {
                ValueRef::Global(id) => *id,
                ValueRef::Local(_) => unreachable!(),
            },
        )));
    };
    if top_objects.contains_key(id) {
        return Err(invalid_static_value(plan));
    }
    let Some(binding) = plan.top_bindings.get(id) else {
        return Err(CompileError::MissingRepresentation(*id));
    };
    let HeapRhs::Bytes(value) = &binding.rhs else {
        return Err(invalid_static_value(plan));
    };
    Ok(plan
        .bytes
        .get(value)
        .ok_or(CompileError::MissingRepresentation(*id))?
        .as_ptr() as usize)
}

fn write_pointer(
    words: &mut [u64],
    offset: usize,
    size: usize,
    value: usize,
) -> Result<(), CompileError> {
    let bytes = value.to_ne_bytes();
    if size != bytes.len() {
        return Err(CompileError::Layout(
            tidepool_repr::execution_schema::LayoutError::InvalidPointerWidth(
                u8::try_from(size.saturating_mul(8)).unwrap_or(u8::MAX),
            ),
        ));
    }
    write_bytes(words, offset, &bytes)
}

fn write_scalar(
    plan: &ProgramPlan<'_>,
    words: &mut [u64],
    offset: usize,
    size: usize,
    literal: &ScalarLiteral,
) -> Result<(), CompileError> {
    let bytes = match literal {
        ScalarLiteral::Int { bytes, .. }
        | ScalarLiteral::Word { bytes, .. }
        | ScalarLiteral::Float { bytes, .. } => bytes,
        _ => return Err(invalid_static_value(plan)),
    };
    if bytes.len() != size {
        return Err(CompileError::Layout(
            tidepool_repr::execution_schema::LayoutError::Overflow,
        ));
    }
    let mut native = bytes.clone();
    if matches!(
        &plan.program.envelope().target.endianness,
        tidepool_repr::execution_schema::Endianness::Little
    ) {
        native.reverse();
    }
    write_bytes(words, offset, &native)
}

fn write_bytes(words: &mut [u64], offset: usize, bytes: &[u8]) -> Result<(), CompileError> {
    let end = offset
        .checked_add(bytes.len())
        .ok_or(CompileError::Static(StaticImageError::Allocation))?;
    let total = words
        .len()
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or(CompileError::Static(StaticImageError::Allocation))?;
    if end > total {
        return Err(CompileError::Static(StaticImageError::Allocation));
    }
    unsafe {
        ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            words.as_mut_ptr().cast::<u8>().add(offset),
            bytes.len(),
        );
    }
    Ok(())
}

fn invalid_static_value(plan: &ProgramPlan<'_>) -> CompileError {
    CompileError::Unsupported(Unsupported::Expression {
        binding: plan.program.entry(),
        node: 0,
    })
}
