pub mod pipeline;
pub mod occ;
pub mod beta;
pub mod case_reduce;
pub mod inline;
pub mod dce;
pub mod partial;

pub use pipeline::{optimize, default_passes, run_pipeline, PipelineStats};
