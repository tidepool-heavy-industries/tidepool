#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
#[path = "../checked_quoter_program.rs"]
mod checked_quoter_program;
#[path = "../prepared_execution.rs"]
mod prepared_execution;
#[path = "../prepared_residency.rs"]
mod prepared_residency;
#[path = "../prepared_resident_composite.rs"]
mod prepared_resident_composite;
#[path = "../prepared_unit_codegen_cost.rs"]
mod prepared_unit_codegen_cost;
