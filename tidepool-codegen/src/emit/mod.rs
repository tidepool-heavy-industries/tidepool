pub mod case;
pub mod expr;
pub mod join;
pub mod primop;

use cranelift_codegen::ir::{FuncRef, SigRef, Value};
use std::collections::HashMap;
use tidepool_repr::{CoreExpr, JoinId, PrimOpKind, VarId};

pub use crate::layout::*;

/// Per-function compilation context bundling common parameters.
pub struct EmitSession<'a> {
    pub pipeline: &'a mut crate::pipeline::CodegenPipeline,
    pub vmctx: Value,
    pub gc_sig: SigRef,
    pub oom_func: FuncRef,
    pub tree: &'a CoreExpr,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailCtx {
    Tail,
    NonTail,
}

impl TailCtx {
    pub fn is_tail(self) -> bool {
        matches!(self, TailCtx::Tail)
    }
}

/// A scoped environment mapping variables to SSA values.
pub struct ScopedEnv {
    inner: HashMap<VarId, SsaVal>,
}

#[allow(clippy::new_without_default)]
impl ScopedEnv {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn get(&self, var: &VarId) -> Option<&SsaVal> {
        self.inner.get(var)
    }

    pub fn contains_key(&self, var: &VarId) -> bool {
        self.inner.contains_key(var)
    }

    pub fn insert(&mut self, var: VarId, val: SsaVal) -> Option<SsaVal> {
        self.inner.insert(var, val)
    }

    /// Restores a variable to its previous state.
    pub fn restore(&mut self, var: VarId, old: Option<SsaVal>) {
        if let Some(val) = old {
            self.inner.insert(var, val);
        } else {
            self.inner.remove(&var);
        }
    }

    pub fn iter(&self) -> std::collections::hash_map::Iter<'_, VarId, SsaVal> {
        self.inner.iter()
    }

    pub fn keys(&self) -> std::collections::hash_map::Keys<'_, VarId, SsaVal> {
        self.inner.keys()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Inserts a variable into the environment and records the old value in the scope.
    pub fn insert_scoped(&mut self, scope: &mut EnvScope, var: VarId, val: SsaVal) {
        let old = self.insert(var, val);
        scope.saved.push((var, old));
    }

    /// Restores all variables saved in the scope in reverse order.
    pub fn restore_scope(&mut self, scope: EnvScope) {
        for (var, old) in scope.saved.into_iter().rev() {
            self.restore(var, old);
        }
    }
}

/// A set of saved environment bindings to be restored.
pub struct EnvScope {
    pub(crate) saved: Vec<(VarId, Option<SsaVal>)>,
}

impl EnvScope {
    pub fn new() -> Self {
        Self { saved: Vec::new() }
    }
}

impl Default for EnvScope {
    fn default() -> Self {
        Self::new()
    }
}

/// Emission context — bundles state during IR generation for one function.
pub struct EmitContext {
    pub env: ScopedEnv,
    pub(crate) join_blocks: JoinPointRegistry,
    pub lambda_counter: u32,
    pub prefix: String,
    /// Storage for LetRec deferred state, indexed by work items.
    pub(crate) letrec_states: Vec<crate::emit::expr::LetRecDeferredState>,
}

pub(crate) struct JoinPointRegistry {
    blocks: HashMap<JoinId, JoinInfo>,
}

impl JoinPointRegistry {
    pub(crate) fn new() -> Self {
        Self {
            blocks: HashMap::new(),
        }
    }

    pub(crate) fn register(&mut self, label: JoinId, info: JoinInfo) {
        self.blocks.insert(label, info);
    }

    pub(crate) fn get(&self, label: &JoinId) -> Result<&JoinInfo, EmitError> {
        self.blocks.get(label).ok_or_else(|| {
            EmitError::NotYetImplemented(format!("Jump to unregistered join {:?}", label))
        })
    }

    pub(crate) fn remove(&mut self, label: &JoinId) -> Option<JoinInfo> {
        self.blocks.remove(label)
    }
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
            env: ScopedEnv::new(),
            join_blocks: JoinPointRegistry::new(),
            lambda_counter: 0,
            prefix,
            letrec_states: Vec::new(),
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
            if let Some(SsaVal::HeapPtr(v)) = self.env.get(&k) {
                builder.declare_value_needs_stack_map(*v);
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

    pub fn next_thunk_name(&mut self) -> String {
        let n = self.lambda_counter;
        self.lambda_counter += 1;
        format!("{}_thunk_{}", self.prefix, n)
    }
}
