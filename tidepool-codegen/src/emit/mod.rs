pub mod case;
pub mod expr;
pub mod join;
pub mod primop;

use cranelift_codegen::ir::{FuncRef, SigRef, Value};
use rustc_hash::FxHashMap;
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
    inner: FxHashMap<VarId, SsaVal>,
}

#[allow(clippy::new_without_default)]
impl ScopedEnv {
    pub fn new() -> Self {
        Self {
            inner: FxHashMap::default(),
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

/// Bundles the three most common parameters for emission functions to reduce
/// argument count and satisfy clippy::too_many_arguments.
pub struct EmitArgs<'a, 'b, 'c> {
    pub ctx: &'a mut EmitContext,
    pub sess: &'a mut EmitSession<'b>,
    pub builder: &'a mut cranelift_frontend::FunctionBuilder<'c>,
}

pub(crate) struct JoinPointRegistry {
    blocks: FxHashMap<JoinId, JoinInfo>,
}

impl JoinPointRegistry {
    pub(crate) fn new() -> Self {
        Self {
            blocks: FxHashMap::default(),
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
#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    #[error("unbound variable: {0:?}")]
    UnboundVariable(VarId),
    #[error("not yet implemented: {0}")]
    NotYetImplemented(String),
    #[error("cranelift error: {0}")]
    CraneliftError(String),
    #[error("pipeline error: {0}")]
    Pipeline(#[from] crate::pipeline::PipelineError),
    #[error("invalid arity for {0:?}: expected {1}, got {2}")]
    InvalidArity(PrimOpKind, usize, usize),
    /// A variable needed for closure capture was not found in the environment.
    #[error("missing capture variable VarId({id:#x}): {ctx}", id = .0.0, ctx = .1)]
    MissingCaptureVar(VarId, String),
    /// Internal invariant violation (should never happen).
    #[error("internal error: {0}")]
    InternalError(String),
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
