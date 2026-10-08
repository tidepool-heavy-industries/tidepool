//! Authored test sources supplied as declared runtime resources.

use std::path::{Component, Path};

/// Read a UTF-8 fixture by its repository-relative name.
///
/// The runner supplies `TIDEPOOL_TEST_FIXTURE_ROOT`. Missing resources fail;
/// frozen executions never consult a mutable source checkout. Each call reads
/// the resource again so an existing test binary observes changed fixture bytes.
pub fn fixture_source(relative: &str) -> String {
    assert!(
        !relative.is_empty()
            && Path::new(relative)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "test fixture name must be a nonempty repository-relative path: {relative}"
    );
    let root = std::env::var_os("TIDEPOOL_TEST_FIXTURE_ROOT")
        .unwrap_or_else(|| panic!("missing declared fixture resource TIDEPOOL_TEST_FIXTURE_ROOT"));
    let path = Path::new(&root).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("test fixture {}: {error}", path.display()))
}
