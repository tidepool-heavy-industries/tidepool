pub mod beta;
pub mod case_reduce;
pub mod dce;
pub mod inline;
pub mod occ;
pub mod partial;
pub mod pipeline;

pub use pipeline::{default_passes, optimize, run_pipeline, PipelineStats};
