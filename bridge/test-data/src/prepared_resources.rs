//! Runtime resources produced by declared prepared-fixture build actions.
//!
//! Reading a resource starts no compiler and never falls back to source-tree
//! snapshots. Tests requiring a compiler scenario use the production compiler.

use std::path::PathBuf;

/// Read one requested target from a declared fixture directory.
pub fn read_target(directory_variable: &str, target: &str) -> Vec<u8> {
    let directory = std::env::var_os(directory_variable)
        .unwrap_or_else(|| panic!("missing declared fixture resource {directory_variable}"));
    let path = PathBuf::from(directory).join(format!("{target}.prepared.cbor"));
    std::fs::read(&path)
        .unwrap_or_else(|error| panic!("prepared fixture {}: {error}", path.display()))
}
