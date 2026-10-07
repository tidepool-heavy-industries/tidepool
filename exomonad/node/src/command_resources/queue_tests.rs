use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};

fn key(name: &str) -> Key {
    ("run-actor".into(), name.into())
}

#[test]
fn protected_commands_progress_without_bypassing_general_fifo() {
    let mut queue = Queue::new(8, 2, 1);
    queue.push(key("running"), 6);
    assert_eq!(queue.next(), Some((key("running"), 6)));
    queue.push(key("large"), 8);
    queue.push(key("medium"), 2);
    for name in ["small-a", "small-b", "small-c"] {
        queue.push(key(name), 1);
    }
    assert_eq!(queue.next(), Some((key("small-a"), 1)));
    assert_eq!(queue.next(), Some((key("small-b"), 1)));
    assert_eq!(queue.next(), None);
    queue.release(&key("running"));
    assert_eq!(queue.next(), Some((key("large"), 8)));
    assert_eq!(queue.next(), None);
    queue.release(&key("small-a"));
    assert_eq!(queue.next(), Some((key("small-c"), 1)));
    queue.release(&key("large"));
    assert_eq!(queue.next(), Some((key("medium"), 2)));
}

#[test]
fn cancellation_releases_capacity_or_removes_waiting_work_once() {
    let mut queue = Queue::new(8, 0, 1);
    queue.push(key("a"), 8);
    queue.push(key("b"), 8);
    queue.push(key("c"), 8);
    assert_eq!(queue.next(), Some((key("a"), 8)));
    queue.release(&key("b"));
    queue.release(&key("a"));
    queue.release(&key("a"));
    assert_eq!(queue.next(), Some((key("c"), 8)));
    assert_eq!(queue.next(), None);
}

#[test]
fn failed_recovery_keeps_the_queued_command_available() {
    let mut queue = Queue::new(2, 0, 0);
    let blocker = key("blocker");
    let recovering = key("recovering");
    queue.push(blocker.clone(), 2);
    assert_eq!(queue.next(), Some((blocker.clone(), 2)));
    queue.push(recovering.clone(), 1);

    assert!(queue.recover_active(recovering.clone(), 1).is_err());
    queue.release(&blocker);
    assert_eq!(queue.next(), Some((recovering, 1)));
}

#[test]
fn admission_and_recovery_histories_reserve_the_same_capacity() {
    for (general, protected, small, active_bytes) in [
        (1, 0, 0, 1),
        (1, 1, 1, 1),
        (u64::MAX, u64::MAX, u64::MAX, u64::MAX),
        (u64::MAX, 0, u64::MAX, u64::MAX),
    ] {
        let active = key("shared-active");
        let mut admitted = Queue::new(general, protected, small);
        admitted.push(active.clone(), active_bytes);
        assert_eq!(admitted.next(), Some((active.clone(), active_bytes)));

        let mut recovered = Queue::new(general, protected, small);
        recovered
            .recover_active(active.clone(), active_bytes)
            .unwrap();
        assert_eq!(admitted.active, recovered.active);
        assert_eq!(admitted.general_used, recovered.general_used);
        assert_eq!(admitted.protected_used, recovered.protected_used);

        for (name, bytes) in [("general", general), ("small", 1)] {
            let waiting = key(name);
            admitted.push(waiting.clone(), bytes);
            recovered.push(waiting, bytes);
        }
        for _ in 0..3 {
            assert_eq!(admitted.next(), recovered.next());
        }
        admitted.release(&active);
        recovered.release(&active);
        loop {
            let left = admitted.next();
            let right = recovered.next();
            assert_eq!(left, right);
            if left.is_none() {
                break;
            }
        }
    }
}

#[derive(Clone, Debug)]
enum QueueOp {
    Push(u64),
    Next,
    Release(u8),
    RecoverWaiting { slot: u8, orphan_bytes: u64 },
    RecoverActive { slot: u8, orphan_bytes: u64 },
    RecoverOrphan(u64),
}

#[derive(Clone, Debug)]
struct QueueCase {
    general: u64,
    protected: u64,
    small: u64,
    operations: Vec<QueueOp>,
    targeted: bool,
}

fn bytes_up_to(general: u64) -> impl Strategy<Value = u64> + Clone {
    prop_oneof![5 => 1u64..=general, 1 => Just(general)]
}

fn queue_operation(general: u64) -> impl Strategy<Value = QueueOp> {
    let bytes = bytes_up_to(general);
    prop_oneof![
        4 => bytes.clone().prop_map(QueueOp::Push),
        3 => Just(QueueOp::Next),
        2 => any::<u8>().prop_map(QueueOp::Release),
        2 => (any::<u8>(), bytes.clone()).prop_map(|(slot, orphan_bytes)| QueueOp::RecoverWaiting { slot, orphan_bytes }),
        2 => (any::<u8>(), bytes.clone()).prop_map(|(slot, orphan_bytes)| QueueOp::RecoverActive { slot, orphan_bytes }),
        2 => bytes.prop_map(QueueOp::RecoverOrphan),
    ]
}

fn random_queue_case() -> impl Strategy<Value = QueueCase> {
    (1u64..=16, 0u64..=8, 0u64..=16).prop_flat_map(|(general, protected, small)| {
        prop::collection::vec(queue_operation(general), 1..40).prop_map(move |operations| {
            QueueCase {
                general,
                protected,
                small,
                operations,
                targeted: false,
            }
        })
    })
}

fn boundary_queue_case() -> impl Strategy<Value = QueueCase> {
    prop_oneof![
        Just((1, 0, 0)),
        Just((u64::MAX, u64::MAX, u64::MAX)),
        Just((u64::MAX, 0, u64::MAX)),
        Just((u64::MAX, u64::MAX, 0)),
    ]
    .prop_flat_map(|(general, protected, small)| {
        prop::collection::vec(queue_operation(general), 1..40).prop_map(move |operations| {
            QueueCase {
                general,
                protected,
                small,
                operations,
                targeted: false,
            }
        })
    })
}

fn targeted_recovery_case() -> impl Strategy<Value = QueueCase> {
    prop::collection::vec(queue_operation(2), 0..8).prop_map(|suffix| {
        let mut operations = vec![
            QueueOp::Push(2),
            QueueOp::Next,
            QueueOp::Push(1),
            QueueOp::RecoverWaiting {
                slot: 0,
                orphan_bytes: 1,
            },
            QueueOp::Release(0),
            QueueOp::Next,
        ];
        operations.extend(suffix);
        QueueCase {
            general: 2,
            protected: 0,
            small: 0,
            operations,
            targeted: true,
        }
    })
}

fn queue_histories() -> impl Strategy<Value = QueueCase> {
    prop_oneof![4 => random_queue_case(), 1 => boundary_queue_case(), 2 => targeted_recovery_case()]
}

#[derive(Clone, Copy, Debug, Default)]
struct QueueCoverage {
    cases: usize,
    targeted_cases: usize,
    operations: usize,
    pushes: usize,
    admissions: usize,
    general_admissions: usize,
    protected_admissions: usize,
    blocked_admissions: usize,
    active_releases: usize,
    waiting_releases: usize,
    absent_releases: usize,
    recoveries: usize,
    waiting_recoveries: usize,
    orphan_recoveries: usize,
    active_recovery_repeats: usize,
    failed_recoveries: usize,
    failed_recovery_then_admitted: usize,
    zero_protected_configs: usize,
    extreme_configs: usize,
}

impl QueueCoverage {
    fn add(&mut self, other: Self) {
        self.cases += other.cases;
        self.targeted_cases += other.targeted_cases;
        self.operations += other.operations;
        self.pushes += other.pushes;
        self.admissions += other.admissions;
        self.general_admissions += other.general_admissions;
        self.protected_admissions += other.protected_admissions;
        self.blocked_admissions += other.blocked_admissions;
        self.active_releases += other.active_releases;
        self.waiting_releases += other.waiting_releases;
        self.absent_releases += other.absent_releases;
        self.recoveries += other.recoveries;
        self.waiting_recoveries += other.waiting_recoveries;
        self.orphan_recoveries += other.orphan_recoveries;
        self.active_recovery_repeats += other.active_recovery_repeats;
        self.failed_recoveries += other.failed_recoveries;
        self.failed_recovery_then_admitted += other.failed_recovery_then_admitted;
        self.zero_protected_configs += other.zero_protected_configs;
        self.extreme_configs += other.extreme_configs;
    }
}

#[derive(Clone, Copy)]
struct Waiting {
    id: u32,
    bytes: u64,
}
#[derive(Clone, Copy)]
struct Active {
    id: u32,
    pool: Pool,
    bytes: u64,
}

struct Ledger {
    general: u64,
    protected: u64,
    small: u64,
    waiting: Vec<Waiting>,
    active: Vec<Active>,
    known: Vec<u32>,
}

impl Ledger {
    fn new(general: u64, protected: u64, small: u64) -> Self {
        Self {
            general,
            protected,
            small,
            waiting: Vec::new(),
            active: Vec::new(),
            known: Vec::new(),
        }
    }
    fn used(&self, pool: Pool) -> u128 {
        self.active
            .iter()
            .filter(|active| active.pool == pool)
            .map(|active| u128::from(active.bytes))
            .sum()
    }
    fn fresh(&mut self, id: u32) {
        assert!(!self.known.contains(&id), "generated queue identity reused");
        self.known.push(id);
    }
    fn push(&mut self, id: u32, bytes: u64) {
        self.fresh(id);
        self.waiting.push(Waiting { id, bytes });
    }
    fn next(&mut self) -> Option<(u32, u64, Pool)> {
        let protected_used = self.used(Pool::Protected);
        let general_used = self.used(Pool::General);
        let protected_position = self.waiting.iter().position(|waiting| {
            waiting.bytes <= self.small
                && protected_used + u128::from(waiting.bytes) <= u128::from(self.protected)
        });
        let (position, pool) = if let Some(position) = protected_position {
            (position, Pool::Protected)
        } else if let Some(head) = self.waiting.first() {
            if general_used + u128::from(head.bytes) <= u128::from(self.general) {
                (0, Pool::General)
            } else {
                return None;
            }
        } else {
            return None;
        };
        let waiting = self.waiting.remove(position);
        self.active.push(Active {
            id: waiting.id,
            pool,
            bytes: waiting.bytes,
        });
        Some((waiting.id, waiting.bytes, pool))
    }
    fn reserve(&mut self, id: u32, bytes: u64) -> Option<Pool> {
        if let Some(active) = self.active.iter().find(|active| active.id == id) {
            return Some(active.pool);
        }
        let protected_fits = bytes <= self.small
            && self.used(Pool::Protected) + u128::from(bytes) <= u128::from(self.protected);
        let general_fits = self.used(Pool::General) + u128::from(bytes) <= u128::from(self.general);
        let pool = if protected_fits {
            Pool::Protected
        } else if general_fits {
            Pool::General
        } else {
            return None;
        };
        if let Some(position) = self.waiting.iter().position(|waiting| waiting.id == id) {
            self.waiting.remove(position);
        } else {
            self.fresh(id);
        }
        self.active.push(Active { id, pool, bytes });
        Some(pool)
    }
    fn release(&mut self, id: u32) -> (bool, bool) {
        let waiting = self.waiting.iter().any(|entry| entry.id == id);
        self.waiting.retain(|entry| entry.id != id);
        let active = self.active.iter().any(|entry| entry.id == id);
        self.active.retain(|entry| entry.id != id);
        (active, waiting)
    }
}

fn queue_key(id: u32) -> Key {
    ("run-actor".into(), format!("job-{id}"))
}
fn queue_id(key: &Key) -> u32 {
    key.1.strip_prefix("job-").unwrap().parse().unwrap()
}

fn check_queue_case(case: &QueueCase) -> Result<QueueCoverage, TestCaseError> {
    let mut queue = Queue::new(case.general, case.protected, case.small);
    let mut ledger = Ledger::new(case.general, case.protected, case.small);
    let mut coverage = QueueCoverage {
        cases: 1,
        targeted_cases: (case.targeted as usize),
        zero_protected_configs: ((case.protected == 0) as usize),
        extreme_configs: usize::from(
            case.general == u64::MAX || case.protected == u64::MAX || case.small == u64::MAX,
        ),
        ..QueueCoverage::default()
    };
    let mut failed_waiting_recovery = None;
    for (index, operation) in case.operations.iter().enumerate() {
        let id = u32::try_from(index).unwrap();
        coverage.operations += 1;
        match *operation {
            QueueOp::Push(bytes) => {
                queue.push(queue_key(id), bytes);
                ledger.push(id, bytes);
                coverage.pushes += 1;
            }
            QueueOp::Next => {
                let expected = ledger.next();
                let actual = queue.next();
                prop_assert_eq!(
                    actual,
                    expected.map(|(id, bytes, _)| (queue_key(id), bytes))
                );
                if let Some((admitted, _, pool)) = expected {
                    coverage.admissions += 1;
                    match pool {
                        Pool::General => coverage.general_admissions += 1,
                        Pool::Protected => coverage.protected_admissions += 1,
                    }
                    if failed_waiting_recovery == Some(admitted) {
                        coverage.failed_recovery_then_admitted += 1;
                        failed_waiting_recovery = None;
                    }
                } else if !ledger.waiting.is_empty() {
                    coverage.blocked_admissions += 1;
                }
            }
            QueueOp::Release(slot) => {
                let target = if ledger.known.is_empty() {
                    u32::MAX - u32::from(slot)
                } else {
                    ledger.known[usize::from(slot) % ledger.known.len()]
                };
                queue.release(&queue_key(target));
                let (active, waiting) = ledger.release(target);
                if active {
                    coverage.active_releases += 1;
                } else if waiting {
                    coverage.waiting_releases += 1;
                } else {
                    coverage.absent_releases += 1;
                }
            }
            QueueOp::RecoverWaiting { slot, orphan_bytes } => {
                coverage.recoveries += 1;
                let (target, bytes, is_waiting) = if ledger.waiting.is_empty() {
                    (id, orphan_bytes, false)
                } else {
                    let entry = ledger.waiting[usize::from(slot) % ledger.waiting.len()];
                    (entry.id, entry.bytes, true)
                };
                let expected = ledger.reserve(target, bytes).is_some();
                let actual = queue.recover_active(queue_key(target), bytes).is_ok();
                prop_assert_eq!(actual, expected);
                if is_waiting {
                    coverage.waiting_recoveries += 1;
                } else {
                    coverage.orphan_recoveries += 1;
                }
                if !expected {
                    coverage.failed_recoveries += 1;
                    failed_waiting_recovery = is_waiting.then_some(target);
                }
            }
            QueueOp::RecoverActive { slot, orphan_bytes } => {
                coverage.recoveries += 1;
                let (target, bytes, repeated) = if ledger.active.is_empty() {
                    (id, orphan_bytes, false)
                } else {
                    let active = ledger.active[usize::from(slot) % ledger.active.len()];
                    (active.id, active.bytes, true)
                };
                let expected = ledger.reserve(target, bytes).is_some();
                let actual = queue.recover_active(queue_key(target), bytes).is_ok();
                prop_assert_eq!(actual, expected);
                coverage.active_recovery_repeats += (repeated as usize);
                coverage.orphan_recoveries += ((!repeated) as usize);
                if !expected {
                    coverage.failed_recoveries += 1;
                }
            }
            QueueOp::RecoverOrphan(bytes) => {
                coverage.recoveries += 1;
                let expected = ledger.reserve(id, bytes).is_some();
                let actual = queue.recover_active(queue_key(id), bytes).is_ok();
                prop_assert_eq!(actual, expected);
                coverage.orphan_recoveries += 1;
                if !expected {
                    coverage.failed_recoveries += 1;
                }
            }
        }
        let actual_waiting = queue
            .waiting
            .iter()
            .map(|(key, bytes)| (queue_id(key), *bytes))
            .collect::<Vec<_>>();
        let model_waiting = ledger
            .waiting
            .iter()
            .map(|entry| (entry.id, entry.bytes))
            .collect::<Vec<_>>();
        prop_assert_eq!(actual_waiting, model_waiting);
        let mut actual_active = queue
            .active
            .iter()
            .map(|(key, (pool, bytes))| (queue_id(key), *pool, *bytes))
            .collect::<Vec<_>>();
        actual_active.sort_by_key(|(id, _, _)| *id);
        let mut model_active = ledger
            .active
            .iter()
            .map(|active| (active.id, active.pool, active.bytes))
            .collect::<Vec<_>>();
        model_active.sort_by_key(|(id, _, _)| *id);
        prop_assert_eq!(actual_active, model_active);
        prop_assert_eq!(u128::from(queue.general_used), ledger.used(Pool::General));
        prop_assert_eq!(
            u128::from(queue.protected_used),
            ledger.used(Pool::Protected)
        );
    }
    Ok(coverage)
}

fn queue_property_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

#[test]
fn generated_queue_histories_match_recomputed_accounting() {
    let mut config = proptest::test_runner::contextualize_config(queue_property_config());
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_queue_histories_match_recomputed_accounting"
    ));
    let configured_cases = config.cases;
    let configured_max_shrink_iters = config.max_shrink_iters;
    let configuration = format!("{config:?}");
    let mut runner = TestRunner::new(config);
    let callback_count = std::cell::Cell::new(0usize);
    let observed = std::cell::RefCell::new(QueueCoverage::default());
    let result = runner.run(&queue_histories(), |case| {
        callback_count.set(callback_count.get() + 1);
        observed.borrow_mut().add(check_queue_case(&case)?);
        Ok(())
    });
    eprintln!(
        "command resource queue observations: configured_cases={configured_cases}, configured_max_shrink_iters={configured_max_shrink_iters}, configuration={configuration}, runner_callbacks_including_replay_and_shrinking={}, successful_history_observations={:?}",
        callback_count.get(),
        *observed.borrow()
    );
    if let Err(error) = result {
        panic!("command resource queue history property failed: {error}");
    }
}
