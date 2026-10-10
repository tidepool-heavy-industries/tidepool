//! Deliberately failing properties consumed only by the persistence selftest.
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};

fn config(direct: bool) -> Config {
    let mut config = Config::default();
    assert!(
        std::path::Path::new(file!()).is_file(),
        "native source identity must resolve in the checkout"
    );
    if direct {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(env!(
            "TIDEPOOL_PROPTEST_REGRESSIONS"
        ))));
    }
    eprintln!("configured_fresh_cases={}", config.cases);
    config
}

fn observe(history: &[u8]) {
    eprintln!("source_identity={}", file!());
    eprintln!("controlled_history={history:?}");
}

proptest! {
    #![proptest_config(config(false))]
    #[test]
    #[ignore = "deliberate failure consumed by the persistence selftest"]
    fn macro_source_parallel(history in prop::collection::vec(7u8..32, 1..8)) {
        observe(&history);
        prop_assert!(history.is_empty(), "controlled history defect");
    }
}

proptest! {
    #![proptest_config(config(true))]
    #[test]
    #[ignore = "deliberate failure consumed by the persistence selftest"]
    fn macro_direct(history in prop::collection::vec(7u8..32, 1..8)) {
        observe(&history);
        prop_assert!(history.is_empty(), "controlled history defect");
    }
}

fn manual(direct: bool, source: bool, name: &'static str) {
    let mut config = config(direct);
    if source {
        config.source_file = Some(file!());
    }
    config.test_name = Some(name);
    eprintln!("manual_source_file={:?}", config.source_file);
    let mut runner = TestRunner::new(config);
    runner
        .run(&prop::collection::vec(7u8..32, 1..8), |history| {
            observe(&history);
            prop_assert!(history.is_empty(), "controlled history defect");
            Ok(())
        })
        .unwrap();
}

#[test]
#[ignore = "deliberate failure consumed by the persistence selftest"]
fn manual_source_parallel() {
    manual(false, true, "manual_source_parallel");
}

#[test]
#[ignore = "deliberate failure consumed by the persistence selftest"]
fn manual_direct() {
    manual(true, true, "manual_direct");
}

#[test]
#[ignore = "deliberate failure consumed by the persistence selftest"]
fn manual_missing_source() {
    manual(false, false, "manual_missing_source");
}
