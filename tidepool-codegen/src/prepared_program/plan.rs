//! Checked binder and closure layout facts used by every emitter consumer.

use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::{HeapBinding, PreparedProgram, RuntimeRep, Signature, ValueId, ValueRef};
use super::CompileError;

pub(super) struct FunctionPlan<'a> {
    pub signature: &'a Signature,
    pub parameters: &'a [ValueId],
    pub captures: &'a [ValueRef],
    pub body: usize,
    pub descriptor: Arc<ObjectDescriptor>,
}

pub(super) struct ProgramPlan<'a> {
    pub program: &'a PreparedProgram,
    pub functions: BTreeMap<ValueId, FunctionPlan<'a>>,
    pub top_bindings: BTreeMap<ValueId, &'a HeapBinding>,
    /// Logical representations, including Void. ValueIds are globally unique
    /// after validation, so no lexical search or scope cloning is necessary.
    pub values: BTreeMap<ValueId, RuntimeRep>,
    pub constructors: Vec<Arc<ObjectDescriptor>>,
    /// Compact slots, not ValueId-indexed allocation controlled by wire IDs.
    pub top_slots: BTreeMap<ValueId, usize>,
}

impl<'a> ProgramPlan<'a> {
    pub fn new(program: &'a PreparedProgram) -> Result<Self, CompileError> {
        // wave4:LAYOUT_PLAN — collect top/local RHS types, function and join
        // parameter reps, case binder/alternative reps from checked signatures
        // and ConstructorDecl. Multi-component case binder is non-value Void.
        // Use authoritative captures, not a second free-variable analysis.
        // Only after all IDs have reps, construct pinned function layouts from
        // capture reps and constructor layouts from their owning declarations.
        todo!("wave4:LAYOUT_PLAN")
    }
}
