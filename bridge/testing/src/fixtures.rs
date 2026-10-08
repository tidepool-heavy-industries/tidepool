//! Authored test sources supplied as declared runtime resources.

use std::path::{Component, Path};

/// Read a UTF-8 fixture by its repository-relative name.
///
/// The runner supplies `TIDEPOOL_TEST_FIXTURE_ROOT`. Missing resources fail;
/// frozen executions never consult a mutable source checkout. Each call reads
/// the resource again so an existing test binary observes changed fixture bytes.
/// Names are checked lexically; declared artifact links in the tree are followed.
pub fn fixture_source(relative: &str) -> String {
    let root = std::env::var_os("TIDEPOOL_TEST_FIXTURE_ROOT")
        .unwrap_or_else(|| panic!("missing declared fixture resource TIDEPOOL_TEST_FIXTURE_ROOT"));
    read_fixture(Path::new(&root), relative)
}

fn read_fixture(root: &Path, relative: &str) -> String {
    assert!(
        !relative.is_empty()
            && Path::new(relative)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "test fixture name must be a nonempty repository-relative path: {relative}"
    );
    let path = root.join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("test fixture {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_binary_reads_changed_fixture_bytes() {
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("Fixture.hs");
        std::fs::write(&fixture, "first\n").unwrap();
        assert_eq!(read_fixture(root.path(), "Fixture.hs"), "first\n");
        std::fs::write(&fixture, "changed λ\n").unwrap();
        assert_eq!(read_fixture(root.path(), "Fixture.hs"), "changed λ\n");
    }

    #[test]
    fn missing_and_non_utf8_fixtures_fail_explicitly() {
        let root = tempfile::tempdir().unwrap();
        assert!(std::panic::catch_unwind(|| read_fixture(root.path(), "missing.hs")).is_err());
        std::fs::write(root.path().join("invalid.hs"), [0xff]).unwrap();
        assert!(std::panic::catch_unwind(|| read_fixture(root.path(), "invalid.hs")).is_err());
    }

    #[test]
    fn fixture_names_refuse_absolute_and_parent_components() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "",
            "/Fixture.hs",
            "../Fixture.hs",
            "nested/../Fixture.hs",
            ".",
        ] {
            assert!(std::panic::catch_unwind(|| read_fixture(root.path(), name)).is_err());
        }
    }
}
