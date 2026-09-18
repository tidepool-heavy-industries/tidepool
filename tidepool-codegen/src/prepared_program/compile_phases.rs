//! Per-compile stage attribution for [`super::CompiledProgram::compile_with`].
//!
//! One `prepared compile` event per compiled program carries every stage's
//! wall time beside the size facts that explain it (declared functions and
//! thunks, Cranelift functions and blocks actually emitted, generated code
//! bytes). A resident session compiles one program per notebook unit, so the
//! event stream answers directly whether a later unit re-does an earlier
//! unit's Cranelift work: identical `functions`/`code_bytes` across units of
//! a session means the same reachable program was regenerated.
//!
//! The event is `info` level under the `tidepool_codegen::prepared_compile`
//! target and costs one `Instant::now()` per stage, so it stays on in normal
//! execution rather than hiding behind a diagnostic knob.

use std::time::{Duration, Instant};

/// Wall time accumulated per named stage of one program compile, in the
/// order the stages run.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CompilePhases {
    pub admit: Duration,
    pub plan: Duration,
    pub static_image: Duration,
    pub pipeline_init: Duration,
    pub declare: Duration,
    pub emit_dispatchers: Duration,
    pub emit_functions: Duration,
    pub emit_thunks: Duration,
    pub emit_enter: Duration,
    pub emit_adapters: Duration,
    pub finalize: Duration,
    pub descriptors: Duration,
}

/// Size facts of the program just compiled, read after `finalize` so the
/// Cranelift counters cover everything that was actually generated.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CompileScale {
    /// `PreparedProgram` function bindings this program declares.
    pub plan_functions: usize,
    /// `PreparedProgram` thunk bindings this program declares.
    pub plan_thunks: usize,
    /// Top-level bindings (the program's own roots).
    pub tops: usize,
    /// Constructor declarations this program carries.
    pub constructors: usize,
    /// Globals this program imports from already-installed programs.
    pub imports: usize,
    /// Cranelift functions `define_function` accepted.
    pub functions_defined: u64,
    /// Cranelift IR blocks across those functions.
    pub blocks_emitted: u64,
    /// Machine-code bytes the JIT module finalized.
    pub code_bytes: u64,
}

/// A running stage clock. `lap` closes the current stage and opens the next.
pub(crate) struct PhaseClock {
    last: Instant,
}

impl PhaseClock {
    pub(crate) fn start() -> Self {
        Self {
            last: Instant::now(),
        }
    }

    /// Time since the previous `lap` (or `start`), restarting the clock.
    pub(crate) fn lap(&mut self) -> Duration {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last);
        self.last = now;
        elapsed
    }
}

fn ms(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

/// Emit one compile's attribution. Called exactly once per successful
/// `compile_with`.
pub(crate) fn record(phases: &CompilePhases, scale: &CompileScale) {
    let emit_total = phases.emit_dispatchers
        + phases.emit_functions
        + phases.emit_thunks
        + phases.emit_enter
        + phases.emit_adapters;
    let total = phases.admit
        + phases.plan
        + phases.static_image
        + phases.pipeline_init
        + phases.declare
        + emit_total
        + phases.finalize
        + phases.descriptors;
    tracing::info!(
        target: "tidepool_codegen::prepared_compile",
        total_ms = ms(total),
        admit_ms = ms(phases.admit),
        plan_ms = ms(phases.plan),
        static_image_ms = ms(phases.static_image),
        pipeline_init_ms = ms(phases.pipeline_init),
        declare_ms = ms(phases.declare),
        emit_ms = ms(emit_total),
        emit_dispatchers_ms = ms(phases.emit_dispatchers),
        emit_functions_ms = ms(phases.emit_functions),
        emit_thunks_ms = ms(phases.emit_thunks),
        emit_enter_ms = ms(phases.emit_enter),
        emit_adapters_ms = ms(phases.emit_adapters),
        finalize_ms = ms(phases.finalize),
        descriptors_ms = ms(phases.descriptors),
        plan_functions = scale.plan_functions,
        plan_thunks = scale.plan_thunks,
        tops = scale.tops,
        constructors = scale.constructors,
        imports = scale.imports,
        functions_defined = scale.functions_defined,
        blocks_emitted = scale.blocks_emitted,
        code_bytes = scale.code_bytes,
        "prepared compile"
    );
}
