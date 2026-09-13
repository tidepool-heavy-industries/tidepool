use super::Unsupported;
use std::collections::BTreeMap;
use tidepool_repr::execution_schema::{Atom, ExprFrame, GlobalId, Group, HeapBinding, HeapRhs, LinkedProgram, SignatureId, ValueRef};

fn items<T>(group: &Group<T>) -> &[T] {
    match group { Group::NonRecursive(item) => std::slice::from_ref(item), Group::Recursive(items) => items }
}

/// Admission is whole-program and precedes declaration/publication. Validation
/// already proved the flat arena's ownership, bounds, scopes and representations.
pub fn admit_program(linked: &LinkedProgram) -> Result<(), Unsupported> {
    let program = linked.prepared();
    if !program.globals().is_empty() { return Err(Unsupported::Global(GlobalId(0))); }
    let mut functions = BTreeMap::new();
    let mut admit_binding = |binding: &HeapBinding| -> Result<(), Unsupported> {
        match &binding.rhs {
            HeapRhs::Thunk { .. } => return Err(Unsupported::Thunk(binding.id)),
            HeapRhs::Function { signature, .. } => { functions.insert(binding.id, *signature); }
            HeapRhs::Constructor { .. } | HeapRhs::Bytes(_) => {}
        }
        Ok(())
    };
    for group in program.bindings() {
        for top in items(group) { admit_binding(&top.binding)?; }
    }
    for frame in &program.expressions().nodes {
        if let ExprFrame::Let { bindings, .. } = frame {
            for binding in items(bindings) { admit_binding(binding)?; }
        }
    }
    for (node, frame) in program.expressions().nodes.iter().enumerate() {
        let rejected = match frame {
            ExprFrame::Operation { .. } => true,
            ExprFrame::Call { callee, signature, .. } => {
                let actual: Option<&SignatureId> = match callee {
                    Atom::Ref(ValueRef::Local(id)) => functions.get(id),
                    _ => None,
                };
                actual.is_none_or(|actual| program.signatures()[actual.0 as usize] != program.signatures()[signature.0 as usize])
            }
            _ => false,
        };
        if rejected {
            // wave4:ADMISSION — use the owning binding, not the entry, in this
            // diagnostic; derive ownership stack-safely from top/local roots.
            return Err(Unsupported::Expression { binding: program.entry(), node });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn admission_rejects_nested_thunks_before_execution() {
        todo!("wave4:ADMISSION_TESTS")
    }
}
