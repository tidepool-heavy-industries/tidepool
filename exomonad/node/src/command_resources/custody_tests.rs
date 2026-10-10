use super::tests::{owner, owner_with_journal};
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseResult, TestRunner};

const CONTROLS: [&str; 3] = ["memory.max", "memory.swap.max", "memory.oom.group"];

fn clear_fake_controls(directory: &Path) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            std::fs::remove_dir(entry.path()).unwrap();
        } else {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
}

#[test]
fn each_control_write_failure_recovers_from_admission_without_reading_controls() {
    for control in CONTROLS {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("commands");
        let journal = temporary.path().join("ownership.jsonl");
        let original = owner_with_journal(&root, journal.clone());
        *original.fail_next_control_write.lock() = Some(control);
        assert!(matches!(
            original.submit("actor-1", "command-1", MIB).unwrap(),
            CommandResourceStatus::CleanupUnconfirmed { .. }
        ));
        let directory = root.join("actor-1/command-1");
        assert!(directory.join(control).is_dir());
        assert!(!directory.join("cgroup.events").exists());
        drop(original);

        let recovered = owner_with_journal(&root, journal.clone());
        let observation = recovered.observation();
        assert_eq!(observation.active, 0);
        assert_eq!(observation.retained_allocations, 1);
        assert_eq!(observation.cleanup_failures, 1);
        assert!(recovered.acknowledge("actor-1", "command-1").is_err());
        assert!(recovered.seal_producer("actor-1").is_err());
        assert!(!matches!(
            recovered.started("actor-1", "command-1").unwrap(),
            CommandResourceStatus::Running
        ));

        clear_fake_controls(&directory);
        recovered.observe();
        assert!(!directory.exists());
        assert_eq!(
            recovered.status("actor-1", "command-1").unwrap(),
            CommandResourceStatus::CancelledBeforeStart
        );
        recovered.acknowledge("actor-1", "command-1").unwrap();
        recovered.seal_producer("actor-1").unwrap();
        drop(recovered);
        let retired = owner_with_journal(&root, journal);
        assert_eq!(
            retired.status("actor-1", "command-1").unwrap(),
            CommandResourceStatus::Retired
        );
        assert!(retired.submit("actor-1", "late", MIB).is_err());
    }
}

#[test]
fn failure_before_leaf_creation_has_no_allocation_to_retain() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("commands");
    let journal = temporary.path().join("ownership.jsonl");
    let resources = owner_with_journal(&root, journal.clone());
    std::fs::create_dir(root.join("actor-1")).unwrap();
    let broken_control = root.join("actor-1/cgroup.subtree_control");
    std::fs::create_dir(&broken_control).unwrap();
    assert!(matches!(
        resources.submit("actor-1", "command-1", MIB).unwrap(),
        CommandResourceStatus::CleanupUnconfirmed { .. }
    ));
    assert!(!root.join("actor-1/command-1").exists());
    assert_eq!(resources.observation().retained_allocations, 0);
    resources.acknowledge("actor-1", "command-1").unwrap();
    resources.seal_producer("actor-1").unwrap();
    std::fs::remove_dir(broken_control).unwrap();
    drop(resources);
    let recovered = owner_with_journal(&root, journal);
    assert_eq!(
        recovered.status("actor-1", "command-1").unwrap(),
        CommandResourceStatus::Retired
    );
}

#[test]
fn empty_unissued_leaf_releases_memory_capacity_while_retaining_cleanup_custody() {
    let root = tempfile::tempdir().unwrap();
    let resources = owner(root.path());
    *resources.fail_next_control_write.lock() = Some("memory.max");
    resources
        .submit("actor-1", "failed", resources.policy.general_bytes)
        .unwrap();
    assert_eq!(resources.observation().retained_allocations, 1);
    assert_eq!(resources.observation().active, 0);
    assert!(matches!(
        resources
            .submit("actor-2", "next", resources.policy.general_bytes)
            .unwrap(),
        CommandResourceStatus::Admitted { .. }
    ));
    assert_eq!(resources.observation().active, 1);
    assert_eq!(resources.observation().retained_allocations, 2);
}

#[test]
fn removed_unissued_leaf_keeps_settlement_custody_until_journal_reopen() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("commands");
    let journal = temporary.path().join("ownership.jsonl");
    let resources = owner_with_journal(&root, journal.clone());
    resources.journal.lock().fail_next_allocation_append();
    assert!(matches!(
        resources.submit("actor-1", "command-1", MIB).unwrap(),
        CommandResourceStatus::CleanupUnconfirmed { .. }
    ));
    let directory = root.join("actor-1/command-1");
    clear_fake_controls(&directory);
    resources.observe();
    assert!(!directory.exists());
    assert_eq!(resources.observation().retained_allocations, 1);
    assert!(resources.acknowledge("actor-1", "command-1").is_err());
    assert!(resources.seal_producer("actor-1").is_err());
    drop(resources);

    let recovered = owner_with_journal(&root, journal);
    assert_eq!(recovered.observation().retained_allocations, 0);
    assert_eq!(
        recovered.status("actor-1", "command-1").unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    );
    recovered.acknowledge("actor-1", "command-1").unwrap();
    recovered.seal_producer("actor-1").unwrap();
}

#[derive(Clone, Debug)]
enum Op {
    Submit { actor: u8, control: u8 },
    Repair(u8),
    Poll,
    Reopen,
    Cancel(u8),
    Start(u8),
    Acknowledge(u8),
    Seal(u8),
}

fn operation() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0u8..3, 0u8..3).prop_map(|(actor, control)| Op::Submit { actor, control }),
        2 => (0u8..3).prop_map(Op::Repair),
        2 => Just(Op::Poll),
        2 => Just(Op::Reopen),
        1 => (0u8..3).prop_map(Op::Cancel),
        1 => (0u8..3).prop_map(Op::Start),
        2 => (0u8..3).prop_map(Op::Acknowledge),
        2 => (0u8..3).prop_map(Op::Seal),
    ]
}

fn histories() -> impl Strategy<Value = Vec<Op>> {
    prop_oneof![
        proptest::collection::vec(operation(), 1..48),
        (
            proptest::collection::vec(operation(), 0..12),
            0u8..3,
            proptest::collection::vec(operation(), 0..12)
        )
            .prop_map(|(mut prefix, control, suffix)| {
                // Actor 3 is reserved for this sequence, so arbitrary prefixes
                // cannot seal it or cancel its first admission.
                prefix.extend([
                    Op::Submit { actor: 3, control },
                    Op::Acknowledge(3),
                    Op::Seal(3),
                    Op::Start(3),
                    Op::Reopen,
                    Op::Repair(3),
                    Op::Poll,
                    Op::Acknowledge(3),
                    Op::Seal(3),
                    Op::Reopen,
                ]);
                prefix.extend(suffix);
                prefix
            }),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelEntry {
    Unseen,
    Blocked,
    ReadyForCleanup,
    Settled,
    Acknowledged,
}

#[derive(Default, Debug)]
struct Coverage {
    submissions: usize,
    control_failures: [usize; 3],
    cleanup_refusals: usize,
    cleanup_successes: usize,
    recovery: usize,
    acknowledgement_refusals: usize,
    seal_refusals: usize,
    starts_refused: usize,
    sealed_submission_refusals: usize,
}

fn check_history(operations: &[Op], observed: &mut Coverage) -> TestCaseResult {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("commands");
    let journal = temporary.path().join("ownership.jsonl");
    let mut resources = owner_with_journal(&root, journal.clone());
    let mut model = [ModelEntry::Unseen; 4];
    let mut sealed = [false; 4];
    for (step, operation) in operations.iter().enumerate() {
        let context = format!("step={step}, operation={operation:?}, history={operations:?}");
        match *operation {
            Op::Submit { actor, control } => {
                let index = usize::from(actor);
                let name = format!("actor-{actor}");
                *resources.fail_next_control_write.lock() = Some(CONTROLS[usize::from(control)]);
                let result = resources.submit(&name, "command-1", MIB);
                *resources.fail_next_control_write.lock() = None;
                if sealed[index] {
                    prop_assert!(result.is_err(), "{}", context);
                    observed.sealed_submission_refusals += 1;
                } else {
                    let status = result.unwrap();
                    prop_assert!(
                        !matches!(
                            status,
                            CommandResourceStatus::Admitted { .. } | CommandResourceStatus::Running
                        ),
                        "{}",
                        context
                    );
                    if model[index] == ModelEntry::Unseen {
                        model[index] = ModelEntry::Blocked;
                        observed.submissions += 1;
                        observed.control_failures[usize::from(control)] += 1;
                    }
                }
            }
            Op::Repair(actor) => {
                let index = usize::from(actor);
                if model[index] == ModelEntry::Blocked {
                    clear_fake_controls(&root.join(format!("actor-{actor}/command-1")));
                    model[index] = ModelEntry::ReadyForCleanup;
                }
            }
            Op::Poll | Op::Reopen => {
                if matches!(operation, Op::Reopen) {
                    drop(resources);
                    resources = owner_with_journal(&root, journal.clone());
                    observed.recovery += 1;
                } else {
                    resources.observe();
                }
                for entry in &mut model {
                    match entry {
                        ModelEntry::ReadyForCleanup => {
                            *entry = ModelEntry::Settled;
                            observed.cleanup_successes += 1;
                        }
                        ModelEntry::Blocked => observed.cleanup_refusals += 1,
                        _ => {}
                    }
                }
            }
            Op::Cancel(actor) => {
                let index = usize::from(actor);
                resources
                    .cancel(&format!("actor-{actor}"), "command-1")
                    .unwrap();
                // Cancellation uses the owner's shared unissued cleanup retry.
                if matches!(
                    model[index],
                    ModelEntry::Blocked | ModelEntry::ReadyForCleanup
                ) {
                    for entry in &mut model {
                        if *entry == ModelEntry::ReadyForCleanup {
                            *entry = ModelEntry::Settled;
                            observed.cleanup_successes += 1;
                        }
                    }
                } else if model[index] == ModelEntry::Unseen {
                    model[index] = ModelEntry::Settled;
                }
            }
            Op::Start(actor) => {
                let index = usize::from(actor);
                let result = resources.started(&format!("actor-{actor}"), "command-1");
                if model[index] == ModelEntry::Unseen {
                    prop_assert!(result.is_err(), "{}", context);
                } else {
                    prop_assert!(
                        !matches!(result.unwrap(), CommandResourceStatus::Running),
                        "{}",
                        context
                    );
                }
                observed.starts_refused += 1;
            }
            Op::Acknowledge(actor) => {
                let index = usize::from(actor);
                let result = resources.acknowledge(&format!("actor-{actor}"), "command-1");
                if matches!(
                    model[index],
                    ModelEntry::Unseen | ModelEntry::Blocked | ModelEntry::ReadyForCleanup
                ) {
                    prop_assert!(result.is_err(), "{}", context);
                    observed.acknowledgement_refusals += 1;
                } else {
                    prop_assert!(result.is_ok(), "{}", context);
                    model[index] = ModelEntry::Acknowledged;
                }
            }
            Op::Seal(actor) => {
                let index = usize::from(actor);
                let result = resources.seal_producer(&format!("actor-{actor}"));
                if matches!(
                    model[index],
                    ModelEntry::Blocked | ModelEntry::ReadyForCleanup
                ) {
                    prop_assert!(result.is_err(), "{}", context);
                    observed.seal_refusals += 1;
                } else {
                    prop_assert!(result.is_ok(), "{}", context);
                    sealed[index] = true;
                    if model[index] != ModelEntry::Unseen {
                        model[index] = ModelEntry::Acknowledged;
                    }
                }
            }
        }
        // Observe indexes without polling, so the oracle does not silently
        // settle a ReadyForCleanup state between generated operations.
        let state = resources.state.lock();
        let expected_retained = model
            .iter()
            .filter(|entry| matches!(entry, ModelEntry::Blocked | ModelEntry::ReadyForCleanup))
            .count();
        prop_assert!(state.active.is_empty(), "{}", context);
        prop_assert_eq!(
            state.retained_allocations.len(),
            expected_retained,
            "{}",
            context
        );
        prop_assert_eq!(
            state.cleanup_failures.len(),
            expected_retained,
            "{}",
            context
        );
        for (index, entry) in model.iter().enumerate() {
            let key = (format!("actor-{index}"), "command-1".into());
            let directory = root.join(&key.0).join(&key.1);
            let retained = matches!(entry, ModelEntry::Blocked | ModelEntry::ReadyForCleanup);
            prop_assert_eq!(directory.is_dir(), retained, "{}", context);
            prop_assert_eq!(
                state.retained_allocations.contains(&key),
                retained,
                "{}",
                context
            );
            prop_assert_eq!(
                state.sealed_producers.contains(&key.0),
                sealed[index],
                "{}",
                context
            );
            if retained {
                prop_assert_eq!(
                    state.entries[&key].directory(),
                    Some(&directory),
                    "{}",
                    context
                );
                prop_assert!(
                    matches!(
                        state.entries[&key].allocation,
                        Some(AllocationCustody::Unissued { .. })
                    ),
                    "{}",
                    context
                );
            }
        }
    }
    Ok(())
}

#[test]
fn generated_unissued_allocation_histories_preserve_custody() {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_unissued_allocation_histories_preserve_custody"
    ));
    let configured_cases = config.cases;
    let mut runner = TestRunner::new(config);
    let callbacks = std::cell::Cell::new(0usize);
    let coverage = std::cell::RefCell::new(Coverage::default());
    let result = runner.run(&histories(), |operations| {
        callbacks.set(callbacks.get() + 1);
        check_history(&operations, &mut coverage.borrow_mut())
    });
    eprintln!(
        "unissued allocation campaign: configured_cases={configured_cases}, callbacks_including_replay_and_shrinking={}, coverage={:?}",
        callbacks.get(),
        coverage.borrow()
    );
    if let Err(error) = result {
        panic!("unissued allocation custody history failed: {error}");
    }
}

#[test]
fn lifecycle_history_support_reaches_each_control_failure_and_cleanup_recovery() {
    let mut coverage = Coverage::default();
    for control in 0..3 {
        check_history(
            &[
                Op::Submit { actor: 0, control },
                Op::Start(0),
                Op::Acknowledge(0),
                Op::Seal(0),
                Op::Poll,
                Op::Reopen,
                Op::Repair(0),
                Op::Poll,
                Op::Acknowledge(0),
                Op::Seal(0),
                Op::Reopen,
                Op::Submit { actor: 0, control },
            ],
            &mut coverage,
        )
        .unwrap();
    }
    assert_eq!(coverage.control_failures, [1; 3]);
    assert_eq!(coverage.cleanup_successes, 3);
    assert_eq!(coverage.acknowledgement_refusals, 3);
    assert_eq!(coverage.seal_refusals, 3);
    assert_eq!(coverage.sealed_submission_refusals, 3);
}
