#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
#[path = "../prepared_reply_types.rs"]
mod prepared_reply_types;
#[path = "../prepared_residency.rs"]
mod prepared_residency;
#[path = "../prepared_unit_codegen_cost.rs"]
mod prepared_unit_codegen_cost;
#[path = "../prepared_execution.rs"]
mod prepared_execution;
#[path = "../prepared_resident_composite.rs"]
mod prepared_resident_composite;
