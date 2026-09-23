#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
#[path = "../support/mod.rs"]
mod support;

#[path = "../exact_source_membrane.rs"]
mod exact_source_membrane;
#[path = "../prepared_render_probe.rs"]
mod prepared_render_probe;
#[path = "../profile_compile_failures.rs"]
mod profile_compile_failures;
#[path = "../ractor_substrate.rs"]
mod ractor_substrate;
#[path = "../resident_local_actor.rs"]
mod resident_local_actor;
