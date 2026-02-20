pub mod expr;
pub mod primop;
pub mod case;
pub mod join;

use cranelift_codegen::ir::Value;
use tidepool_repr::{VarId, JoinId, PrimOpKind};
use std::collections::HashMap;

// HeapObject layout constants
pub const HEAP_HEADER_SIZE: u64 = 8;
pub const CLOSURE_CODE_PTR_OFFSET: i32 = 8;
pub const CLOSURE_NUM_CAPTURED_OFFSET: i32 = 16;
pub const CLOSURE_CAPTURED_START: i32 = 24;
pub const CON_TAG_OFFSET: i32 = 8;
pub const CON_NUM_FIELDS_OFFSET: i32 = 16;
pub const CON_FIELDS_START: i32 = 24;
pub const LIT_TAG_OFFSET: i32 = 8;
pub const LIT_VALUE_OFFSET: i32 = 16;
pub const LIT_TOTAL_SIZE: u64 = 24;
pub const LIT_TAG_INT: i64 = 0;
pub const LIT_TAG_WORD: i64 = 1;
pub const LIT_TAG_CHAR: i64 = 2;
pub const LIT_TAG_FLOAT: i64 = 3;
pub const LIT_TAG_DOUBLE: i64 = 4;
pub const LIT_TAG_STRING: i64 = 5;

/// SSA value with boxed/unboxed tracking.
#[derive(Debug, Clone, Copy)]
pub enum SsaVal {
    /// Unboxed raw value (i64 or f64 bits) with its literal tag.
    Raw(Value, i64),
    /// Heap pointer. Already declared via `declare_value_needs_stack_map`.
    HeapPtr(Value),
}

impl SsaVal {
    pub fn value(self) -> Value {
        match self {
            SsaVal::Raw(v, _) | SsaVal::HeapPtr(v) => v,
        }
    }
}

/// Emission context — bundles state during IR generation for one function.
pub struct EmitContext {
    pub env: HashMap<VarId, SsaVal>,
    pub join_blocks: HashMap<JoinId, JoinInfo>,
    pub lambda_counter: u32,
    pub prefix: String,
}

/// Placeholder for join point info (used by case/join leaf later).
pub struct JoinInfo {
    pub block: cranelift_codegen::ir::Block,
    pub param_types: Vec<SsaVal>,
}

/// Errors during IR emission.
#[derive(Debug)]
pub enum EmitError {
    UnboundVariable(VarId),
    NotYetImplemented(String),
    CraneliftError(String),
    InvalidArity(PrimOpKind, usize, usize),
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmitError::UnboundVariable(v) => write!(f, "unbound variable: {:?}", v),
            EmitError::NotYetImplemented(s) => write!(f, "not yet implemented: {}", s),
            EmitError::CraneliftError(s) => write!(f, "cranelift error: {}", s),
            EmitError::InvalidArity(op, expected, got) => {
                write!(f, "invalid arity for {:?}: expected {}, got {}", op, expected, got)
            }
        }
    }
}

impl std::error::Error for EmitError {}

impl EmitContext {
    pub fn new(prefix: String) -> Self {
        Self {
            env: HashMap::new(),
            join_blocks: HashMap::new(),
            lambda_counter: 0,
            prefix,
        }
    }

    pub fn next_lambda_name(&mut self) -> String {
        let n = self.lambda_counter;
        self.lambda_counter += 1;
        format!("{}_lambda_{}", self.prefix, n)
    }
}