//! Reference execution for validated prepared-STG programs.

use std::collections::BTreeMap;

use tidepool_repr::execution_schema::{
    ConstructorId, ImportedValue, LinkedProgram, OperationId, ScalarLiteral, SymbolIdentity,
};

mod runtime;

/// Observable values produced by the prepared-STG reference evaluator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceValue {
    Scalar(ScalarLiteral),
    Constructor {
        constructor: ConstructorId,
        fields: Vec<ReferenceValue>,
    },
    Void,
}

/// A materialized import value and the resident generation which owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceImport {
    pub generation: u64,
    pub value: ReferenceValue,
}

/// Runtime values supplied for globals whose contracts were already linked.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReferenceImports {
    pub values: BTreeMap<SymbolIdentity, ReferenceImport>,
}

/// Structured operation semantics are selected by dense operation ID, never by
/// rendered operation names inside the evaluator.
pub trait ReferenceOperations {
    fn call(
        &mut self,
        operation: OperationId,
        arguments: &[ReferenceValue],
    ) -> Result<Vec<ReferenceValue>, ReferenceError>;
}

#[derive(Default)]
pub struct NoReferenceOperations;

impl ReferenceOperations for NoReferenceOperations {
    fn call(
        &mut self,
        operation: OperationId,
        _arguments: &[ReferenceValue],
    ) -> Result<Vec<ReferenceValue>, ReferenceError> {
        Err(ReferenceError::UnsupportedOperation(operation))
    }
}

/// Bounded evaluator inputs. Imported values are immutable for an execution;
/// failures cannot mutate the linked program or its import snapshot.
pub struct ReferenceExecution<'a> {
    pub imports: &'a ReferenceImports,
    pub operations: &'a mut dyn ReferenceOperations,
    pub fuel: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReferenceError {
    #[error("reference evaluator exhausted its fuel")]
    FuelExhausted,
    #[error("missing linked import value {0:?}")]
    MissingImport(SymbolIdentity),
    #[error("linked import metadata does not match program globals")]
    InvalidLinkedImports,
    #[error("unknown local value {0:?}")]
    UnknownValue(tidepool_repr::execution_schema::ValueId),
    #[error("unknown global value {0:?}")]
    UnknownGlobal(tidepool_repr::execution_schema::GlobalId),
    #[error("unknown join {0:?}")]
    UnknownJoin(tidepool_repr::execution_schema::JoinId),
    #[error("blackholed thunk {0:?}")]
    Blackhole(tidepool_repr::execution_schema::ValueId),
    #[error("single-entry closure or thunk entered more than once")]
    SingleEntryReentered,
    #[error("attempted to call a non-function value")]
    NotCallable,
    #[error("reference call arity mismatch: expected {expected}, got {actual}")]
    Arity { expected: usize, actual: usize },
    #[error("reference result arity mismatch: expected {expected}, got {actual}")]
    ResultArity { expected: usize, actual: usize },
    #[error("no case alternative matched")]
    NoAlternative,
    #[error("case binder count mismatch: expected {expected}, got {actual}")]
    CaseBinders { expected: usize, actual: usize },
    #[error("unsupported reference operation {0:?}")]
    UnsupportedOperation(OperationId),
    #[error("reference evaluator cannot expose a function as a result")]
    FunctionResult,
    #[error("invalid scalar encoding: {0}")]
    InvalidScalar(&'static str),
}

/// Execute a program which has crossed the sole validation and linking
/// boundary. Initial and imported state are borrowed and never mutated.
pub fn execute_linked(
    program: &LinkedProgram,
    execution: &mut ReferenceExecution<'_>,
) -> Result<Vec<ReferenceValue>, ReferenceError> {
    validate_linked_imports(
        program.imports(),
        program.prepared().globals(),
        program.prepared().signatures(),
    )?;
    runtime::execute(program, execution)
}

fn validate_linked_imports(
    imports: &[ImportedValue],
    globals: &[tidepool_repr::execution_schema::GlobalDecl],
    signatures: &[tidepool_repr::execution_schema::Signature],
) -> Result<(), ReferenceError> {
    if imports.len() != globals.len()
        || imports.iter().zip(globals).any(|(value, declaration)| {
            value.identity != declaration.identity
                || value.signature != signatures[declaration.signature.0 as usize]
        })
    {
        return Err(ReferenceError::InvalidLinkedImports);
    }
    Ok(())
}
