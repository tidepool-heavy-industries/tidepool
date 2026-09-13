use std::sync::{Arc, atomic::AtomicBool};
use tidepool_bridge::Value;
use tidepool_repr::execution_schema::ValueId;
use crate::machine_state::MachineFailure;
use super::{CompiledProgram, ObservationFailure, Unsupported};

pub struct RunOptions {
    pub nursery_bytes: usize,
    pub observation_budget: usize,
    /// Contract-test/diagnostic request, using the ordinary collector after
    /// native unwind and result-root admission, before nonforcing observation.
    pub collect_before_observation: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self { nursery_bytes: 4096, observation_budget: 100_000, collect_before_observation: false }
    }
}

#[derive(Debug)]
pub struct RunResult {
    pub values: Vec<Value>,
    pub collections: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("entry {0:?} is not exported by this program")]
    MissingEntry(ValueId),
    #[error("entry arguments: expected {expected} physical scalar slots, got {actual}")]
    Arguments { expected: usize, actual: usize },
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    #[error("{cause}", cause = .0.cause)]
    Runtime(MachineFailure),
    #[error(transparent)]
    Observation(#[from] ObservationFailure),
    #[error(transparent)]
    Static(#[from] tidepool_heap::static_region::StaticImageError),
}

impl CompiledProgram {
    pub fn run_entry(
        &self,
        _entry: ValueId,
        _arguments: &[u64],
        _options: &RunOptions,
        _cancel: Arc<AtomicBool>,
    ) -> Result<RunResult, ExecutionError> {
        // wave4:INVOCATION — scalar-only host admission, private static
        // instantiation/top table, fresh MachineState with code/maps/descriptors
        // pinned through unwind, ordinary prepared nursery, vector C adapter.
        // Check status/first cause BEFORE registering or reading result slots.
        // Register managed result layout slots before optional collection;
        // restore root mark on every exit. Observe while storage remains owned.
        // Integrity in observation retires machine with first cause retained.
        // No compiled address or returned Value may retain an invocation pointer.
        todo!("wave4:INVOCATION")
    }
}
