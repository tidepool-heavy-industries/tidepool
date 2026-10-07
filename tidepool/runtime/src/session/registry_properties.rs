use super::*;

use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

const IDS: usize = 3;
const HOLES: usize = 4;

#[derive(Debug)]
struct FakeMachine {
    identity: u8,
    value: i16,
    drops: Arc<AtomicUsize>,
}

impl Drop for FakeMachine {
    fn drop(&mut self) {
        self.drops.fetch_add(1, AtomicOrdering::SeqCst);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Hole(u8);

#[derive(Debug, Clone, Copy)]
enum Request {
    Run,
    Resume,
    Child,
}

#[derive(Debug, Clone, Copy)]
enum Completion {
    DropRestore,
    Restore,
    SettleSuspended,
    SettleWedged,
    SettleRetire,
}

#[derive(Debug, Clone)]
enum Op {
    Insert(u8, u8, i16),
    TryInsert(u8, u8, i16),
    Checkout(u8, Request, Completion, Vec<u8>, i16),
    Remove(u8, u8),
    StaleReplace(u8, u8, i16, i16, i16),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let hole_list = prop::collection::vec(0_u8..HOLES as u8, 0..5);
    let request = prop_oneof![
        Just(Request::Run),
        Just(Request::Resume),
        Just(Request::Child),
    ];
    let completion = prop_oneof![
        Just(Completion::DropRestore),
        Just(Completion::Restore),
        Just(Completion::SettleSuspended),
        Just(Completion::SettleWedged),
        Just(Completion::SettleRetire),
    ];
    prop_oneof![
        3 => (0_u8..IDS as u8, 0_u8..8, -4_i16..5).prop_map(|(i,m,v)| Op::Insert(i,m,v)),
        2 => (0_u8..IDS as u8, 0_u8..8, -4_i16..5).prop_map(|(i,m,v)| Op::TryInsert(i,m,v)),
        8 => (0_u8..IDS as u8, request, completion, hole_list, -3_i16..4)
            .prop_map(|(i,r,c,h,v)| Op::Checkout(i,r,c,h,v)),
        3 => (0_u8..IDS as u8, 0_u8..8).prop_map(|(i,r)| Op::Remove(i,r)),
        2 => (0_u8..IDS as u8, 0_u8..8, -4_i16..5, -4_i16..5, -4_i16..5)
            .prop_map(|(i,a,b,x,y)| Op::StaleReplace(i,a,b,x,y)),
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelSlot {
    Idle(u8, i16),
    Running(Vec<Hole>),
    Suspended(u8, i16, Vec<Hole>),
    Wedged,
}

#[derive(Default)]
struct Model {
    slots: HashMap<u8, ModelSlot>,
    // Keep the full event stream. The oracle applies the ring bound at query
    // time instead of copying the production eviction algorithm.
    tombstones: Vec<(u8, String)>,
    drops: usize,
}

#[derive(Default, Debug)]
struct Coverage {
    inserted: usize,
    try_refused: usize,
    removed: usize,
    checkout_succeeded: [usize; 3],
    checkout_refused: [usize; 6],
    completion: [usize; 5],
    stale_suspended: usize,
    tombstone_evictions: usize,
    duplicate_holes: usize,
}

impl Coverage {
    fn accumulate(&mut self, other: &Self) {
        self.inserted += other.inserted;
        self.try_refused += other.try_refused;
        self.removed += other.removed;
        self.stale_suspended += other.stale_suspended;
        self.tombstone_evictions += other.tombstone_evictions;
        self.duplicate_holes += other.duplicate_holes;
        for (total, count) in self
            .checkout_succeeded
            .iter_mut()
            .zip(other.checkout_succeeded)
        {
            *total += count;
        }
        for (total, count) in self.checkout_refused.iter_mut().zip(other.checkout_refused) {
            *total += count;
        }
        for (total, count) in self.completion.iter_mut().zip(other.completion) {
            *total += count;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    Unknown(u8),
    Retired(u8, String),
    Running(u8),
    Terminal(u8),
    NotSuspended(u8),
    WrongHole(u8, Hole, Vec<Hole>),
}

fn retirement(model: &Model, id: u8) -> Option<String> {
    model
        .tombstones
        .iter()
        .rev()
        .take(super::TOMBSTONE_CAPACITY)
        .find(|(candidate, _)| *candidate == id)
        .map(|(_, why)| why.clone())
}

fn record_retirement(model: &mut Model, evictions: &mut usize, id: u8, why: String) {
    if model.tombstones.len() >= super::TOMBSTONE_CAPACITY {
        *evictions += 1;
    }
    model.tombstones.push((id, why));
}

fn has_duplicate(holes: &[Hole]) -> bool {
    holes
        .iter()
        .enumerate()
        .any(|(index, hole)| holes[..index].contains(hole))
}

fn refusal_index(refusal: &Refusal) -> usize {
    match refusal {
        Refusal::Running(_) => 0,
        Refusal::Terminal(_) => 1,
        Refusal::Unknown(_) => 2,
        Refusal::Retired(_, _) => 3,
        Refusal::NotSuspended(_) => 4,
        Refusal::WrongHole(_, _, _) => 5,
    }
}

fn model_checkout(
    model: &Model,
    id: u8,
    request: Request,
    hole: Hole,
) -> Result<(u8, i16, Vec<Hole>), Refusal> {
    let Some(slot) = model.slots.get(&id) else {
        return Err(
            retirement(model, id).map_or(Refusal::Unknown(id), |why| Refusal::Retired(id, why))
        );
    };
    let (machine, value, holes) = match slot {
        ModelSlot::Running(_) => return Err(Refusal::Running(id)),
        ModelSlot::Wedged => return Err(Refusal::Terminal(id)),
        ModelSlot::Idle(m, v) => (*m, *v, Vec::new()),
        ModelSlot::Suspended(m, v, hs) => (*m, *v, hs.clone()),
    };
    if matches!(request, Request::Resume) && !holes.contains(&hole) {
        return Err(Refusal::WrongHole(id, hole, holes));
    }
    if matches!(request, Request::Child) && holes.is_empty() {
        return Err(Refusal::NotSuspended(id));
    }
    Ok((machine, value, holes))
}

fn machine(id: u8, value: i16, drops: &Arc<AtomicUsize>) -> Box<FakeMachine> {
    Box::new(FakeMachine {
        identity: id,
        value,
        drops: Arc::clone(drops),
    })
}

fn actual_error(error: CheckoutError<Hole>) -> Refusal {
    match error {
        CheckoutError::Unknown(SessionId(id)) => Refusal::Unknown(id as u8),
        CheckoutError::Retired {
            session: SessionId(id),
            reason,
        } => Refusal::Retired(id as u8, reason),
        CheckoutError::Running(SessionId(id)) => Refusal::Running(id as u8),
        CheckoutError::Terminal {
            session: SessionId(id),
            ..
        } => Refusal::Terminal(id as u8),
        CheckoutError::NotSuspended(SessionId(id)) => Refusal::NotSuspended(id as u8),
        CheckoutError::WrongHole {
            session: SessionId(id),
            attempted,
            parked,
        } => Refusal::WrongHole(id as u8, attempted, parked),
        other => panic!("unexpected sequential checkout error: {other:?}"),
    }
}

fn check_state(reg: &SessionRegistry<FakeMachine, Hole>, model: &Model, drops: &AtomicUsize) {
    for id in 0..IDS as u8 {
        let expected = model.slots.get(&id);
        let actual_kind = reg.kind(SessionId(id as u64));
        let expected_kind = expected.map(|s| match s {
            ModelSlot::Idle(..) => SlotKind::Idle,
            ModelSlot::Running(..) => SlotKind::Running,
            ModelSlot::Suspended(..) => SlotKind::Suspended,
            ModelSlot::Wedged => SlotKind::Wedged,
        });
        assert_eq!(actual_kind, expected_kind, "slot kind for id {id}");
        let peeked = reg.peek(SessionId(id as u64), |m| (m.identity, m.value));
        let expected_machine = expected.and_then(|s| match s {
            ModelSlot::Idle(m, v) | ModelSlot::Suspended(m, v, _) => Some((*m, *v)),
            _ => None,
        });
        assert_eq!(peeked, expected_machine, "present machine for id {id}");
        let expected_holes = expected.and_then(|s| match s {
            ModelSlot::Suspended(_, _, holes) | ModelSlot::Running(holes) => Some(holes.clone()),
            _ => None,
        });
        let actual_holes =
            reg.slots
                .lock()
                .get(&SessionId(id as u64))
                .and_then(|entry| match &entry.slot {
                    Slot::Running { holes } | Slot::Suspended { holes, .. } => Some(holes.clone()),
                    Slot::Idle(_) | Slot::Wedged { .. } => None,
                });
        assert_eq!(actual_holes, expected_holes, "ordered holes for id {id}");
    }
    assert_eq!(
        drops.load(AtomicOrdering::SeqCst),
        model.drops,
        "machine drop count"
    );
}

fn replay(ops: &[Op]) -> Coverage {
    let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut model = Model::default();
    let mut coverage = Coverage::default();
    for op in ops {
        match op {
            Op::Insert(id, identity, value) => {
                coverage.inserted += 1;
                if model
                    .slots
                    .insert(*id, ModelSlot::Idle(*identity, *value))
                    .is_some_and(|old| match old {
                        ModelSlot::Idle(..) | ModelSlot::Suspended(..) => true,
                        _ => false,
                    })
                {
                    model.drops += 1;
                }
                drop(reg.insert_idle(SessionId(*id as u64), machine(*identity, *value, &drops)));
            }
            Op::TryInsert(id, identity, value) => {
                let result =
                    reg.try_insert_idle(SessionId(*id as u64), machine(*identity, *value, &drops));
                if model.slots.contains_key(id) {
                    coverage.try_refused += 1;
                    let returned = result.expect_err("occupied logical id must refuse insertion");
                    assert_eq!((returned.identity, returned.value), (*identity, *value));
                    drop(returned);
                    model.drops += 1;
                } else {
                    result.expect("vacant logical id accepts insertion");
                    model.slots.insert(*id, ModelSlot::Idle(*identity, *value));
                }
            }
            Op::Remove(id, why) => {
                if let Some(old) = model.slots.remove(id) {
                    coverage.removed += 1;
                    if matches!(old, ModelSlot::Idle(..) | ModelSlot::Suspended(..)) {
                        model.drops += 1;
                    }
                    record_retirement(
                        &mut model,
                        &mut coverage.tombstone_evictions,
                        *id,
                        format!("retire-{why}"),
                    );
                }
                drop(reg.remove(SessionId(*id as u64), format!("retire-{why}")));
            }
            Op::Checkout(id, request, completion, new_holes, mutation) => {
                let request_index = match request {
                    Request::Run => 0,
                    Request::Resume => 1,
                    Request::Child => 2,
                };
                let selected_hole = Hole((request_index as u8 + *id) % HOLES as u8);
                let model_result = model_checkout(&model, *id, *request, selected_hole.clone());
                match model_result {
                    Err(expected) => {
                        coverage.checkout_refused[refusal_index(&expected)] += 1;
                        let result = match request {
                            Request::Run => reg.checkout_run(SessionId(*id as u64)),
                            Request::Resume => {
                                reg.checkout_resume(SessionId(*id as u64), &selected_hole)
                            }
                            Request::Child => reg.checkout_child(SessionId(*id as u64)),
                        };
                        let error = match result {
                            Err(error) => error,
                            Ok(checkout) => {
                                drop(checkout);
                                panic!("model refused checkout but registry admitted it");
                            }
                        };
                        assert_eq!(actual_error(error), expected);
                    }
                    Ok((machine_id, value, carried)) => {
                        coverage.checkout_succeeded[request_index] += 1;
                        let result = match request {
                            Request::Run => reg.checkout_run(SessionId(*id as u64)),
                            Request::Resume => {
                                reg.checkout_resume(SessionId(*id as u64), &selected_hole)
                            }
                            Request::Child => reg.checkout_child(SessionId(*id as u64)),
                        };
                        let mut checkout = result.expect("model admitted checkout");
                        assert_eq!(checkout.holes_at_checkout(), carried.as_slice());
                        checkout.machine().value += *mutation;
                        let value = value + *mutation;
                        match completion {
                            Completion::DropRestore => {
                                coverage.completion[0] += 1;
                                coverage.duplicate_holes += usize::from(has_duplicate(&carried));
                                drop(checkout);
                                model.slots.insert(
                                    *id,
                                    if carried.is_empty() {
                                        ModelSlot::Idle(machine_id, value)
                                    } else {
                                        ModelSlot::Suspended(machine_id, value, carried)
                                    },
                                );
                            }
                            Completion::Restore => {
                                coverage.completion[1] += 1;
                                let hs = new_holes.iter().copied().map(Hole).collect::<Vec<_>>();
                                coverage.duplicate_holes += usize::from(has_duplicate(&hs));
                                checkout.restore_suspended(hs.clone());
                                model.slots.insert(
                                    *id,
                                    if hs.is_empty() {
                                        ModelSlot::Idle(machine_id, value)
                                    } else {
                                        ModelSlot::Suspended(machine_id, value, hs)
                                    },
                                );
                            }
                            Completion::SettleSuspended => {
                                coverage.completion[2] += 1;
                                let (m, receipt) = checkout.into_parts();
                                let hs = new_holes.iter().copied().map(Hole).collect::<Vec<_>>();
                                coverage.duplicate_holes += usize::from(has_duplicate(&hs));
                                reg.settle_suspended(receipt, m, hs.clone());
                                model.slots.insert(
                                    *id,
                                    if hs.is_empty() {
                                        ModelSlot::Idle(machine_id, value)
                                    } else {
                                        ModelSlot::Suspended(machine_id, value, hs)
                                    },
                                );
                            }
                            Completion::SettleWedged => {
                                coverage.completion[3] += 1;
                                let (_, receipt) = checkout.into_parts();
                                reg.settle_wedged(receipt, Instant::now());
                                model.drops += 1;
                                model.slots.insert(*id, ModelSlot::Wedged);
                            }
                            Completion::SettleRetire => {
                                coverage.completion[4] += 1;
                                let (m, receipt) = checkout.into_parts();
                                drop(m);
                                reg.settle_retire(receipt, "settled-retire");
                                model.drops += 1;
                                model.slots.remove(id);
                                record_retirement(
                                    &mut model,
                                    &mut coverage.tombstone_evictions,
                                    *id,
                                    "settled-retire".into(),
                                );
                            }
                        }
                    }
                }
            }
            Op::StaleReplace(id, replacement, value, mutation, reason) => {
                match model_checkout(&model, *id, Request::Run, Hole(0)) {
                    Ok((machine_id, old_value, carried)) => {
                        let mut checkout = reg
                            .checkout_run(SessionId(*id as u64))
                            .expect("run before stale replacement");
                        model.slots.insert(*id, ModelSlot::Running(carried.clone()));
                        let expected =
                            model_checkout(&model, *id, Request::Run, Hole(0)).unwrap_err();
                        let actual = reg.checkout_run(SessionId(*id as u64));
                        let error = match actual {
                            Err(error) => error,
                            Ok(second) => {
                                drop(second);
                                panic!("a second sequential checkout was admitted");
                            }
                        };
                        assert_eq!(actual_error(error), expected);
                        coverage.checkout_refused[refusal_index(&expected)] += 1;
                        checkout.machine().value += *mutation;
                        let (old_machine, receipt) = checkout.into_parts();
                        if let Some(old) = model.slots.remove(id) {
                            if matches!(old, ModelSlot::Idle(..) | ModelSlot::Suspended(..)) {
                                model.drops += 1;
                            }
                            record_retirement(
                                &mut model,
                                &mut coverage.tombstone_evictions,
                                *id,
                                format!("retire-{reason}"),
                            );
                        }
                        drop(reg.remove(SessionId(*id as u64), format!("retire-{reason}")));
                        coverage.removed += 1;
                        drop(reg.insert_idle(
                            SessionId(*id as u64),
                            machine(*replacement, *value, &drops),
                        ));
                        if model
                            .slots
                            .insert(*id, ModelSlot::Idle(*replacement, *value))
                            .is_some()
                        {
                            model.drops += 1;
                        }
                        reg.settle_suspended(receipt, old_machine, carried);
                        model.drops += 1;
                        coverage.stale_suspended += 1;
                        let _ = (machine_id, old_value);
                    }
                    Err(expected) => {
                        let actual = reg.checkout_run(SessionId(*id as u64));
                        let error = match actual {
                            Err(error) => error,
                            Ok(checkout) => {
                                drop(checkout);
                                panic!("stale replacement precondition refused by model only");
                            }
                        };
                        assert_eq!(actual_error(error), expected);
                    }
                }
            }
        }
        check_state(&reg, &model, &drops);
    }
    coverage
}

fn config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

#[derive(Debug, Clone, Copy)]
enum Settlement {
    Suspended,
    Wedged,
    Retire,
}

#[derive(Debug, Clone)]
enum ReceiptOp {
    Insert(u8, i16),
    Detach(u8, Request, u8, i16),
    Observe,
    Remove(u8),
    Settle(u8, Settlement, Vec<u8>, u8),
}

#[derive(Debug)]
struct HeldReceiptModel {
    identity: u8,
    value: i16,
    holes: Vec<Hole>,
    birth_operation: usize,
}

struct HeldReceipt {
    receipt: CheckoutReceipt,
    machine: Box<FakeMachine>,
}

#[derive(Default, Debug)]
struct ReceiptCoverage {
    detached: usize,
    busy_handle_refusals: usize,
    missing_receipt_refusals: usize,
    checkout_refusals: [usize; 6],
    live_settlements: [usize; 3],
    stale_settlements: [usize; 3],
    stale_remove_events: usize,
    tombstone_evictions: usize,
    replacements_while_running: usize,
    stale_into_running: [usize; 3],
    duplicate_checkout_holes: usize,
    duplicate_settlement_holes: usize,
    peak_held_receipts: usize,
    observations: usize,
}

impl ReceiptCoverage {
    fn accumulate(&mut self, other: &Self) {
        self.detached += other.detached;
        self.busy_handle_refusals += other.busy_handle_refusals;
        self.missing_receipt_refusals += other.missing_receipt_refusals;
        self.stale_remove_events += other.stale_remove_events;
        self.tombstone_evictions += other.tombstone_evictions;
        self.replacements_while_running += other.replacements_while_running;
        self.duplicate_checkout_holes += other.duplicate_checkout_holes;
        self.duplicate_settlement_holes += other.duplicate_settlement_holes;
        self.peak_held_receipts = self.peak_held_receipts.max(other.peak_held_receipts);
        self.observations += other.observations;
        for (total, count) in self
            .checkout_refusals
            .iter_mut()
            .zip(other.checkout_refusals)
        {
            *total += count;
        }
        for (total, count) in self.live_settlements.iter_mut().zip(other.live_settlements) {
            *total += count;
        }
        for (total, count) in self
            .stale_settlements
            .iter_mut()
            .zip(other.stale_settlements)
        {
            *total += count;
        }
        for (total, count) in self
            .stale_into_running
            .iter_mut()
            .zip(other.stale_into_running)
        {
            *total += count;
        }
    }
}

fn request_strategy() -> impl Strategy<Value = Request> {
    prop_oneof![
        Just(Request::Run),
        Just(Request::Resume),
        Just(Request::Child)
    ]
}

fn receipt_op_strategy() -> impl Strategy<Value = ReceiptOp> {
    let holes = prop::collection::vec(0_u8..HOLES as u8, 0..5);
    let settlement = settlement_strategy();
    prop_oneof![
        3 => (0_u8..8, -4_i16..5).prop_map(|(i,v)| ReceiptOp::Insert(i,v)),
        4 => (0_u8..2, request_strategy(), 0_u8..HOLES as u8, -3_i16..4)
            .prop_map(|(h,r,hole,v)| ReceiptOp::Detach(h,r,hole,v)),
        1 => Just(ReceiptOp::Observe),
        3 => (0_u8..8).prop_map(ReceiptOp::Remove),
        5 => (0_u8..2, settlement, holes, 0_u8..8).prop_map(|(h,s,hs,r)| ReceiptOp::Settle(h,s,hs,r)),
    ]
}

fn settlement_strategy() -> impl Strategy<Value = Settlement> {
    prop_oneof![
        Just(Settlement::Suspended),
        Just(Settlement::Wedged),
        Just(Settlement::Retire),
    ]
}

fn settlement_index(settlement: Settlement) -> usize {
    match settlement {
        Settlement::Suspended => 0,
        Settlement::Wedged => 1,
        Settlement::Retire => 2,
    }
}

fn next_settlement(settlement: Settlement) -> Settlement {
    match settlement {
        Settlement::Suspended => Settlement::Wedged,
        Settlement::Wedged => Settlement::Retire,
        Settlement::Retire => Settlement::Suspended,
    }
}

fn replay_receipts(ops: &[ReceiptOp]) -> ReceiptCoverage {
    replay_receipts_with_observation(ops, true)
}

fn replay_receipts_with_observation(ops: &[ReceiptOp], observe_each: bool) -> ReceiptCoverage {
    let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut model = Model::default();
    // Incarnation identity is the insert's position in the input history. It
    // does not depend on the registry's epoch counter or allocation results.
    let mut model_births = HashMap::<u8, usize>::new();
    let mut model_receipts = (0..4)
        .map(|_| None)
        .collect::<Vec<Option<HeldReceiptModel>>>();
    let mut receipts = (0..4).map(|_| None).collect::<Vec<Option<HeldReceipt>>>();
    let mut coverage = ReceiptCoverage::default();

    for (operation_index, op) in ops.iter().enumerate() {
        match op {
            ReceiptOp::Insert(identity, value) => {
                coverage.replacements_while_running +=
                    usize::from(matches!(model.slots.get(&0), Some(ModelSlot::Running(_))));
                if model
                    .slots
                    .insert(0, ModelSlot::Idle(*identity, *value))
                    .is_some_and(|old| {
                        matches!(old, ModelSlot::Idle(..) | ModelSlot::Suspended(..))
                    })
                {
                    model.drops += 1;
                }
                model_births.insert(0, operation_index);
                drop(reg.insert_idle(SessionId(0), machine(*identity, *value, &drops)));
            }
            ReceiptOp::Remove(reason) => {
                if let Some(old) = model.slots.remove(&0) {
                    if matches!(old, ModelSlot::Idle(..) | ModelSlot::Suspended(..)) {
                        model.drops += 1;
                    }
                    if model_receipts.iter().any(Option::is_some) {
                        coverage.stale_remove_events += 1;
                    }
                    record_retirement(
                        &mut model,
                        &mut coverage.tombstone_evictions,
                        0,
                        format!("receipt-remove-{reason}"),
                    );
                    model_births.remove(&0);
                }
                drop(reg.remove(SessionId(0), format!("receipt-remove-{reason}")));
            }
            ReceiptOp::Observe => {
                check_state(&reg, &model, &drops);
                coverage.observations += 1;
            }
            ReceiptOp::Detach(handle, request, hole, delta) => {
                if model_receipts[*handle as usize].is_some() {
                    coverage.busy_handle_refusals += 1;
                    if observe_each {
                        check_state(&reg, &model, &drops);
                    }
                    continue;
                }
                match model_checkout(&model, 0, *request, Hole(*hole)) {
                    Err(expected) => {
                        coverage.checkout_refusals[refusal_index(&expected)] += 1;
                        let actual = match request {
                            Request::Run => reg.checkout_run(SessionId(0)),
                            Request::Resume => reg.checkout_resume(SessionId(0), &Hole(*hole)),
                            Request::Child => reg.checkout_child(SessionId(0)),
                        };
                        let error = match actual {
                            Err(error) => error,
                            Ok(checkout) => {
                                drop(checkout);
                                panic!("receipt model refused checkout but registry admitted it");
                            }
                        };
                        assert_eq!(actual_error(error), expected);
                    }
                    Ok((identity, value, holes)) => {
                        let mut checkout = match request {
                            Request::Run => reg.checkout_run(SessionId(0)),
                            Request::Resume => reg.checkout_resume(SessionId(0), &Hole(*hole)),
                            Request::Child => reg.checkout_child(SessionId(0)),
                        }
                        .expect("model admits detach");
                        assert_eq!(checkout.holes_at_checkout(), holes.as_slice());
                        assert_eq!(
                            (checkout.machine().identity, checkout.machine().value),
                            (identity, value)
                        );
                        coverage.duplicate_checkout_holes += usize::from(has_duplicate(&holes));
                        checkout.machine().value += *delta;
                        let (machine, receipt) = checkout.into_parts();
                        model.slots.insert(0, ModelSlot::Running(holes.clone()));
                        model_receipts[*handle as usize] = Some(HeldReceiptModel {
                            identity,
                            value: value + *delta,
                            holes,
                            birth_operation: *model_births
                                .get(&0)
                                .expect("live entry has an insertion"),
                        });
                        receipts[*handle as usize] = Some(HeldReceipt { receipt, machine });
                        coverage.detached += 1;
                        coverage.peak_held_receipts = coverage
                            .peak_held_receipts
                            .max(model_receipts.iter().filter(|held| held.is_some()).count());
                    }
                }
            }
            ReceiptOp::Settle(handle, settlement, new_holes, reason) => {
                let Some(held_model) = model_receipts[*handle as usize].take() else {
                    coverage.missing_receipt_refusals += 1;
                    if observe_each {
                        check_state(&reg, &model, &drops);
                    }
                    continue;
                };
                let held = receipts[*handle as usize]
                    .take()
                    .expect("model and real receipt handles agree");
                let current = model_births.get(&0).copied() == Some(held_model.birth_operation);
                assert_eq!(
                    (held.machine.identity, held.machine.value),
                    (held_model.identity, held_model.value)
                );
                if !current && matches!(model.slots.get(&0), Some(ModelSlot::Running(_))) {
                    coverage.stale_into_running[settlement_index(*settlement)] += 1;
                }
                match settlement {
                    Settlement::Suspended => {
                        let holes = new_holes.iter().copied().map(Hole).collect::<Vec<_>>();
                        coverage.duplicate_settlement_holes += usize::from(has_duplicate(&holes));
                        reg.settle_suspended(held.receipt, held.machine, holes.clone());
                        if current {
                            model.slots.insert(
                                0,
                                if holes.is_empty() {
                                    ModelSlot::Idle(held_model.identity, held_model.value)
                                } else {
                                    ModelSlot::Suspended(
                                        held_model.identity,
                                        held_model.value,
                                        holes,
                                    )
                                },
                            );
                            coverage.live_settlements[0] += 1;
                        } else {
                            model.drops += 1;
                            coverage.stale_settlements[0] += 1;
                        }
                    }
                    Settlement::Wedged => {
                        drop(held.machine);
                        reg.settle_wedged(held.receipt, Instant::now());
                        model.drops += 1;
                        if current {
                            model.slots.insert(0, ModelSlot::Wedged);
                            coverage.live_settlements[1] += 1;
                        } else {
                            coverage.stale_settlements[1] += 1;
                        }
                    }
                    Settlement::Retire => {
                        drop(held.machine);
                        reg.settle_retire(held.receipt, format!("receipt-retire-{reason}"));
                        model.drops += 1;
                        if current {
                            model.slots.remove(&0);
                            record_retirement(
                                &mut model,
                                &mut coverage.tombstone_evictions,
                                0,
                                format!("receipt-retire-{reason}"),
                            );
                            model_births.remove(&0);
                            coverage.live_settlements[2] += 1;
                        } else {
                            coverage.stale_settlements[2] += 1;
                        }
                    }
                }
            }
        }
        if observe_each {
            check_state(&reg, &model, &drops);
        }
    }
    check_state(&reg, &model, &drops);

    // A generated prefix can leave receipts outstanding. Explicitly restore
    // each remaining one so the debug receipt guard never becomes cleanup.
    for handle in 0..receipts.len() {
        if let Some(held) = receipts[handle].take() {
            let held_model = model_receipts[handle]
                .take()
                .expect("receipt model survives cleanup");
            let current = model_births.get(&0).copied() == Some(held_model.birth_operation);
            reg.settle_suspended(held.receipt, held.machine, held_model.holes.clone());
            if current {
                model.slots.insert(
                    0,
                    if held_model.holes.is_empty() {
                        ModelSlot::Idle(held_model.identity, held_model.value)
                    } else {
                        ModelSlot::Suspended(
                            held_model.identity,
                            held_model.value,
                            held_model.holes,
                        )
                    },
                );
            } else {
                model.drops += 1;
            }
        }
    }
    check_state(&reg, &model, &drops);
    let retained = model
        .slots
        .values()
        .filter(|slot| matches!(slot, ModelSlot::Idle(..) | ModelSlot::Suspended(..)))
        .count();
    drop(reg);
    assert_eq!(
        drops.load(AtomicOrdering::SeqCst),
        model.drops + retained,
        "registry shutdown releases every remaining machine exactly once"
    );
    coverage
}

fn delayed_receipt_history(stale: Settlement, suffix: &[ReceiptOp]) -> Vec<ReceiptOp> {
    let live = next_settlement(stale);
    let mut ops = vec![
        ReceiptOp::Insert(1, 10),
        ReceiptOp::Detach(0, Request::Run, 0, 2),
        ReceiptOp::Detach(0, Request::Run, 0, 1), // explicit occupied logical handle refusal
        ReceiptOp::Remove(1),
        ReceiptOp::Insert(2, 20),
        ReceiptOp::Settle(0, stale, vec![3, 1, 1], 2),
        ReceiptOp::Insert(3, 30),
        ReceiptOp::Detach(1, Request::Run, 0, -1),
        ReceiptOp::Settle(1, live, vec![2, 2, 0], 3),
        ReceiptOp::Settle(1, live, vec![], 4), // explicit missing logical receipt
    ];
    ops.extend_from_slice(suffix);
    ops
}

#[derive(Debug, Clone)]
struct ReplacementPattern {
    stale: Settlement,
    live: Settlement,
    request: Request,
    remove_first: bool,
    stale_first: bool,
    hole: u8,
    value: i16,
    delta: i16,
}

fn replacement_pattern_strategy() -> impl Strategy<Value = ReplacementPattern> {
    (
        settlement_strategy(),
        settlement_strategy(),
        request_strategy(),
        any::<bool>(),
        any::<bool>(),
        0_u8..HOLES as u8,
        -4_i16..5,
        -3_i16..4,
    )
        .prop_map(
            |(stale, live, request, remove_first, stale_first, hole, value, delta)| {
                ReplacementPattern {
                    stale,
                    live,
                    request,
                    remove_first,
                    stale_first,
                    hole,
                    value,
                    delta,
                }
            },
        )
}

fn replacement_receipt_history(
    prefix: &[ReceiptOp],
    pattern: &ReplacementPattern,
    suffix: &[ReceiptOp],
) -> Vec<ReceiptOp> {
    // Random surroundings use handles 0 and 1. Reserved handles 2 and 3 keep
    // the causal overlap valid even when shrinking removes prefix operations.
    let holes = vec![pattern.hole, pattern.hole, (pattern.hole + 1) % HOLES as u8];
    let mut ops = prefix.to_vec();
    ops.extend([
        ReceiptOp::Insert(1, pattern.value),
        ReceiptOp::Detach(2, Request::Run, pattern.hole, 0),
        ReceiptOp::Settle(2, Settlement::Suspended, holes.clone(), 0),
        ReceiptOp::Detach(2, pattern.request, pattern.hole, pattern.delta),
        ReceiptOp::Detach(2, Request::Run, pattern.hole, 0),
        ReceiptOp::Detach(3, Request::Run, pattern.hole, 0),
    ]);
    if pattern.remove_first {
        ops.push(ReceiptOp::Remove(0));
    }
    // Unequal payloads make stale overwrite observable independently of kind.
    ops.extend([
        ReceiptOp::Insert(2, pattern.value + 32),
        ReceiptOp::Detach(3, Request::Run, pattern.hole, -pattern.delta),
    ]);
    let stale = ReceiptOp::Settle(2, pattern.stale, holes.clone(), 1);
    let live = ReceiptOp::Settle(3, pattern.live, holes, 2);
    if pattern.stale_first {
        ops.extend([stale, ReceiptOp::Observe, live]);
    } else {
        ops.extend([live, ReceiptOp::Observe, stale]);
    }
    ops.extend([
        ReceiptOp::Observe,
        ReceiptOp::Settle(2, pattern.stale, vec![], 3),
    ]);
    ops.extend_from_slice(suffix);
    ops
}

#[test]
fn deterministic_support_covers_overlapping_replacement_receipts() {
    for stale in [
        Settlement::Suspended,
        Settlement::Wedged,
        Settlement::Retire,
    ] {
        for live in [
            Settlement::Suspended,
            Settlement::Wedged,
            Settlement::Retire,
        ] {
            for request in [Request::Run, Request::Resume, Request::Child] {
                for remove_first in [false, true] {
                    for stale_first in [false, true] {
                        for observe_each in [false, true] {
                            let pattern = ReplacementPattern {
                                stale,
                                live,
                                request,
                                remove_first,
                                stale_first,
                                hole: 1,
                                value: 10,
                                delta: 2,
                            };
                            let coverage = replay_receipts_with_observation(
                                &replacement_receipt_history(&[], &pattern, &[]),
                                observe_each,
                            );
                            assert_eq!(
                                coverage.stale_settlements[settlement_index(stale)],
                                1,
                                "{pattern:?} {coverage:?}"
                            );
                            assert!(
                                coverage.live_settlements[settlement_index(live)] > 0,
                                "{pattern:?} {coverage:?}"
                            );
                            assert_eq!(
                                coverage.stale_into_running[settlement_index(stale)],
                                usize::from(stale_first),
                                "{pattern:?} {coverage:?}"
                            );
                            assert_eq!(
                                coverage.replacements_while_running,
                                usize::from(!remove_first),
                                "{pattern:?} {coverage:?}"
                            );
                            assert_eq!(coverage.peak_held_receipts, 2, "{pattern:?} {coverage:?}");
                            assert_eq!(
                                coverage.duplicate_checkout_holes, 1,
                                "{pattern:?} {coverage:?}"
                            );
                            assert_eq!(
                                coverage.busy_handle_refusals, 1,
                                "{pattern:?} {coverage:?}"
                            );
                            assert_eq!(
                                coverage.missing_receipt_refusals, 1,
                                "{pattern:?} {coverage:?}"
                            );
                            assert_eq!(
                                coverage.checkout_refusals[0], 1,
                                "{pattern:?} {coverage:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn deterministic_generated_receipt_distribution_reports_observed_coverage() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let histories = (
        prop::collection::vec(receipt_op_strategy(), 0..12),
        replacement_pattern_strategy(),
        prop::collection::vec(receipt_op_strategy(), 0..12),
        any::<bool>(),
    );
    let mut coverage = ReceiptCoverage::default();
    for _ in 0..256 {
        let (prefix, pattern, suffix, observe_each) =
            histories.new_tree(&mut runner).unwrap().current();
        let operations = replacement_receipt_history(&prefix, &pattern, &suffix);
        coverage.accumulate(&replay_receipts_with_observation(&operations, observe_each));
    }
    // Partition support is proved by the exhaustive test above. These counts
    // describe this fixed sample rather than imposing a random frequency gate.
    eprintln!("registry deterministic generated receipt coverage: {coverage:?}");
}

#[test]
fn deterministic_support_covers_registry_lifecycle_observations() {
    let operations = [
        Op::Insert(0, 1, 10),
        Op::Checkout(0, Request::Child, Completion::DropRestore, vec![], 0),
        Op::Checkout(0, Request::Resume, Completion::DropRestore, vec![], 0),
        Op::TryInsert(0, 2, 20),
        Op::Checkout(0, Request::Run, Completion::Restore, vec![1, 1, 2], 3),
        Op::Checkout(
            0,
            Request::Child,
            Completion::DropRestore,
            vec![2, 1, 1],
            -1,
        ),
        Op::Checkout(0, Request::Resume, Completion::SettleSuspended, vec![], 1),
        Op::StaleReplace(0, 4, 99, 5, 7),
        Op::Checkout(0, Request::Run, Completion::SettleWedged, vec![], 1),
        Op::Checkout(0, Request::Run, Completion::DropRestore, vec![], 0),
        Op::Insert(0, 5, 12),
        Op::Checkout(0, Request::Run, Completion::SettleRetire, vec![], 2),
        Op::Remove(0, 3),
        Op::Checkout(0, Request::Run, Completion::DropRestore, vec![], 0),
        Op::Checkout(2, Request::Run, Completion::DropRestore, vec![], 0),
    ];
    let support = replay(&operations);
    assert!(support.inserted >= 2 && support.try_refused >= 1 && support.removed >= 1);
    assert!(
        support.checkout_succeeded.iter().all(|count| *count > 0),
        "{support:?}"
    );
    assert!(
        support.checkout_refused.iter().all(|count| *count > 0),
        "{support:?}"
    );
    assert!(
        support.completion.iter().all(|count| *count > 0),
        "{support:?}"
    );
    assert!(
        support.stale_suspended > 0 && support.duplicate_holes > 0,
        "{support:?}"
    );

    let mut retirements = Vec::new();
    retirements.push(Op::Insert(0, 0, 0));
    retirements.push(Op::Remove(0, 0));
    for event in 0..super::TOMBSTONE_CAPACITY {
        retirements.push(Op::Insert(1, event as u8, event as i16));
        retirements.push(Op::Remove(1, event as u8));
    }
    retirements.push(Op::Checkout(
        0,
        Request::Run,
        Completion::DropRestore,
        vec![],
        0,
    ));
    let support = replay(&retirements);
    assert!(support.tombstone_evictions > 0, "{support:?}");
}

#[test]
fn deterministic_support_covers_each_delayed_receipt_settlement() {
    for stale in [
        Settlement::Suspended,
        Settlement::Wedged,
        Settlement::Retire,
    ] {
        let coverage = replay_receipts(&delayed_receipt_history(stale, &[]));
        assert!(
            coverage.stale_settlements[settlement_index(stale)] > 0,
            "{coverage:?}"
        );
        assert!(
            coverage.live_settlements[settlement_index(next_settlement(stale))] > 0,
            "{coverage:?}"
        );
        assert!(
            coverage.busy_handle_refusals > 0 && coverage.missing_receipt_refusals > 0,
            "{coverage:?}"
        );
        assert!(coverage.stale_remove_events > 0, "{coverage:?}");
    }
}

#[test]
fn deterministic_generated_distribution_reaches_registry_transitions() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let histories = prop::collection::vec(op_strategy(), 1..32);
    let mut coverage = Coverage::default();
    for _ in 0..256 {
        let operations = histories.new_tree(&mut runner).unwrap().current();
        coverage.accumulate(&replay(&operations));
    }

    eprintln!("registry deterministic generated coverage: {coverage:?}");
    assert!(
        coverage.inserted > 0 && coverage.try_refused > 0 && coverage.removed > 0,
        "{coverage:?}"
    );
    assert!(
        coverage.checkout_succeeded.iter().all(|count| *count > 0),
        "{coverage:?}"
    );
    assert!(
        coverage.checkout_refused.iter().all(|count| *count > 0),
        "{coverage:?}"
    );
    assert!(
        coverage.completion.iter().all(|count| *count > 0),
        "{coverage:?}"
    );
    assert!(
        coverage.stale_suspended > 0 && coverage.duplicate_holes > 0,
        "{coverage:?}"
    );
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn generated_session_registry_histories_match_logical_model(
        ops in prop::collection::vec(op_strategy(), 1..32)
    ) {
        let _coverage = replay(&ops);
    }

    #[test]
    fn generated_overlapping_replacement_receipts_preserve_current_owner(
        prefix in prop::collection::vec(receipt_op_strategy(), 0..12),
        pattern in replacement_pattern_strategy(),
        suffix in prop::collection::vec(receipt_op_strategy(), 0..12),
        observe_each in any::<bool>(),
    ) {
        let operations = replacement_receipt_history(&prefix, &pattern, &suffix);
        let coverage = replay_receipts_with_observation(&operations, observe_each);
        prop_assert!(coverage.stale_settlements[settlement_index(pattern.stale)] >= 1, "{coverage:?}");
        prop_assert!(coverage.live_settlements[settlement_index(pattern.live)] >= 1, "{coverage:?}");
        prop_assert!(coverage.duplicate_checkout_holes >= 1, "{coverage:?}");
        prop_assert!(coverage.peak_held_receipts >= 2, "{coverage:?}");
        if pattern.stale_first {
            prop_assert!(coverage.stale_into_running[settlement_index(pattern.stale)] >= 1, "{coverage:?}");
        }
    }

    #[test]
    fn generated_unstructured_receipts_match_logical_owner_history(
        operations in prop::collection::vec(receipt_op_strategy(), 0..64),
        observe_each in any::<bool>(),
    ) {
        let _coverage = replay_receipts_with_observation(&operations, observe_each);
    }

    #[test]
    fn generated_delayed_receipt_histories_match_epoch_model(
        stale in settlement_strategy(),
        suffix in prop::collection::vec(receipt_op_strategy(), 0..12),
    ) {
        let ops = delayed_receipt_history(stale, &suffix);
        let coverage = replay_receipts(&ops);
        let stale_index = settlement_index(stale);
        let live_index = settlement_index(next_settlement(stale));
        prop_assert!(coverage.detached >= 2, "{coverage:?}");
        prop_assert!(coverage.busy_handle_refusals >= 1, "{coverage:?}");
        prop_assert!(coverage.missing_receipt_refusals >= 1, "{coverage:?}");
        prop_assert!(coverage.stale_settlements[stale_index] >= 1, "{coverage:?}");
        prop_assert!(coverage.live_settlements[live_index] >= 1, "{coverage:?}");
        prop_assert!(coverage.stale_remove_events >= 1, "{coverage:?}");
        prop_assert!(coverage.checkout_refusals.iter().sum::<usize>() <= ops.len());
        prop_assert!(coverage.tombstone_evictions <= ops.len());
    }
}

#[test]
fn deterministic_tombstone_histories_cover_retirement_event_horizon() {
    let mut below_boundary = vec![Op::Insert(0, 0, 0), Op::Remove(0, 0)];
    for event in 0..(super::TOMBSTONE_CAPACITY - 1) {
        below_boundary.push(Op::Insert(1, event as u8, event as i16));
        below_boundary.push(Op::Remove(1, event as u8));
    }
    below_boundary.push(Op::Checkout(
        0,
        Request::Run,
        Completion::DropRestore,
        vec![],
        0,
    ));
    let coverage = replay(&below_boundary);
    assert_eq!(coverage.tombstone_evictions, 0, "{coverage:?}");
    assert!(coverage.checkout_refused[3] > 0, "{coverage:?}");

    for extra_events in super::TOMBSTONE_CAPACITY..(super::TOMBSTONE_CAPACITY + 12) {
        let mut ops = vec![Op::Insert(0, 0, 0), Op::Remove(0, 0)];
        for event in 0..extra_events {
            ops.push(Op::Insert(1, event as u8, event as i16));
            ops.push(Op::Remove(1, event as u8));
        }
        ops.push(Op::Checkout(
            0,
            Request::Run,
            Completion::DropRestore,
            vec![],
            0,
        ));
        let coverage = replay(&ops);
        assert!(coverage.tombstone_evictions > 0, "{coverage:?}");
        assert!(coverage.checkout_refused[2] > 0, "{coverage:?}");
    }
}
