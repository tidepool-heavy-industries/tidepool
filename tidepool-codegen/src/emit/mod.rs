pub mod case;
pub mod expr;
pub mod join;
pub mod primop;

use cranelift_codegen::ir::Value;
use std::collections::HashMap;
use tidepool_repr::{JoinId, PrimOpKind, VarId};

// HeapObject layout constants
pub const HEAP_HEADER_SIZE: u64 = 8;
pub const CLOSURE_CODE_PTR_OFFSET: i32 = 8;
pub const CLOSURE_NUM_CAPTURED_OFFSET: i32 = 16;
pub const CLOSURE_CAPTURED_START: i32 = 24;
pub const CON_TAG_OFFSET: i32 = 8;
pub const CON_NUM_FIELDS_OFFSET: i32 = 16;
pub const CON_FIELDS_START: i32 = 24;
// -- Thunk layout constants (i32 offsets for Cranelift) --
pub const THUNK_STATE_OFFSET: i32 = 8;
pub const THUNK_CODE_PTR_OFFSET: i32 = 16;
pub const THUNK_CAPTURED_START: i32 = 24;
pub const LIT_TAG_OFFSET: i32 = 8;
pub const LIT_VALUE_OFFSET: i32 = 16;
pub const LIT_TOTAL_SIZE: u64 = 24;
pub const LIT_TAG_INT: i64 = 0;
pub const LIT_TAG_WORD: i64 = 1;
pub const LIT_TAG_CHAR: i64 = 2;
pub const LIT_TAG_FLOAT: i64 = 3;
pub const LIT_TAG_DOUBLE: i64 = 4;
pub const LIT_TAG_STRING: i64 = 5;
pub const LIT_TAG_ADDR: i64 = 6;
pub const LIT_TAG_BYTEARRAY: i64 = 7;
pub const LIT_TAG_SMALLARRAY: i64 = 8;
pub const LIT_TAG_ARRAY: i64 = 9;

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
    pub depth: usize,
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
    Pipeline(crate::pipeline::PipelineError),
    InvalidArity(PrimOpKind, usize, usize),
    /// A variable needed for closure capture was not found in the environment.
    MissingCaptureVar(VarId, String),
    /// Internal invariant violation (should never happen).
    InternalError(String),
    /// Recursion depth limit exceeded during compilation
    DepthLimitExceeded,
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmitError::UnboundVariable(v) => write!(f, "unbound variable: {:?}", v),
            EmitError::NotYetImplemented(s) => write!(f, "not yet implemented: {}", s),
            EmitError::CraneliftError(s) => write!(f, "cranelift error: {}", s),
            EmitError::Pipeline(e) => write!(f, "pipeline error: {}", e),
            EmitError::InvalidArity(op, expected, got) => {
                write!(
                    f,
                    "invalid arity for {:?}: expected {}, got {}",
                    op, expected, got
                )
            }
            EmitError::MissingCaptureVar(v, ctx) => {
                write!(f, "missing capture variable VarId({:#x}): {}", v.0, ctx)
            }
            EmitError::InternalError(msg) => write!(f, "internal error: {}", msg),
            EmitError::DepthLimitExceeded => write!(f, "recursion depth limit exceeded during compilation"),
        }
    }
}

impl std::error::Error for EmitError {}

impl From<crate::pipeline::PipelineError> for EmitError {
    fn from(e: crate::pipeline::PipelineError) -> Self {
        EmitError::Pipeline(e)
    }
}

impl EmitContext {
    pub fn new(prefix: String) -> Self {
        Self {
            env: HashMap::new(),
            join_blocks: HashMap::new(),
            lambda_counter: 0,
            prefix,
            depth: 0,
        }
    }

    /// Re-declare all heap pointers currently in the environment as needing
    /// stack map entries. Should be called after switching to a new block
    /// (e.g., merge blocks, join points, case alternatives) to ensure
    /// liveness is tracked correctly across block boundaries.
    pub fn declare_env(&self, builder: &mut cranelift_frontend::FunctionBuilder) {
        // Collect and sort keys for deterministic IR output (useful for debugging/tests)
        let mut keys: Vec<_> = self.env.keys().collect();
        keys.sort_by_key(|v| v.0);
        for &k in keys {
            if let SsaVal::HeapPtr(v) = self.env[&k] {
                builder.declare_value_needs_stack_map(v);
            }
        }
    }

    pub fn trace_scope(&self, msg: &str) {
        if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
            eprintln!("[scope:{}] {}", self.prefix, msg);
        }
    }

    pub fn next_lambda_name(&mut self) -> String {
        let n = self.lambda_counter;
        self.lambda_counter += 1;
        format!("{}_lambda_{}", self.prefix, n)
    }
}
