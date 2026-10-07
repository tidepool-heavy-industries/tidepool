use super::*;

use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

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
    Detach(u8, i16),
    Remove(u8),
    Settle(u8, Settlement, Vec<u8>, u8),
}

#[derive(Debug)]
struct HeldReceiptModel {
    identity: u8,
    value: i16,
    holes: Vec<Hole>,
    epoch: u64,
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
}

fn receipt_op_strategy() -> impl Strategy<Value = ReceiptOp> {
    let holes = prop::collection::vec(0_u8..HOLES as u8, 0..5);
    let settlement = settlement_strategy();
    prop_oneof![
        3 => (0_u8..8, -4_i16..5).prop_map(|(i,v)| ReceiptOp::Insert(i,v)),
        4 => (0_u8..2, -3_i16..4).prop_map(|(h,v)| ReceiptOp::Detach(h,v)),
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
    let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut model = Model::default();
    let mut model_epochs = HashMap::<u8, u64>::new();
    let mut next_epoch = 1_u64;
    let mut model_receipts = (0..2)
        .map(|_| None)
        .collect::<Vec<Option<HeldReceiptModel>>>();
    let mut receipts = (0..2).map(|_| None).collect::<Vec<Option<HeldReceipt>>>();
    let mut coverage = ReceiptCoverage::default();

    for op in ops {
        match op {
            ReceiptOp::Insert(identity, value) => {
                if model
                    .slots
                    .insert(0, ModelSlot::Idle(*identity, *value))
                    .is_some_and(|old| {
                        matches!(old, ModelSlot::Idle(..) | ModelSlot::Suspended(..))
                    })
                {
                    model.drops += 1;
                }
                model_epochs.insert(0, next_epoch);
                next_epoch += 1;
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
                    model_epochs.remove(&0);
                }
                drop(reg.remove(SessionId(0), format!("receipt-remove-{reason}")));
            }
            ReceiptOp::Detach(handle, delta) => {
                if model_receipts[*handle as usize].is_some() {
                    coverage.busy_handle_refusals += 1;
                    check_state(&reg, &model, &drops);
                    continue;
                }
                match model_checkout(&model, 0, Request::Run, Hole(0)) {
                    Err(expected) => {
                        coverage.checkout_refusals[refusal_index(&expected)] += 1;
                        let actual = reg.checkout_run(SessionId(0));
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
                        let mut checkout =
                            reg.checkout_run(SessionId(0)).expect("model admits detach");
                        checkout.machine().value += *delta;
                        let (machine, receipt) = checkout.into_parts();
                        model.slots.insert(0, ModelSlot::Running(holes.clone()));
                        model_receipts[*handle as usize] = Some(HeldReceiptModel {
                            identity,
                            value: value + *delta,
                            holes,
                            epoch: *model_epochs.get(&0).expect("live entry has epoch"),
                        });
                        receipts[*handle as usize] = Some(HeldReceipt { receipt, machine });
                        coverage.detached += 1;
                    }
                }
            }
            ReceiptOp::Settle(handle, settlement, new_holes, reason) => {
                let Some(held_model) = model_receipts[*handle as usize].take() else {
                    coverage.missing_receipt_refusals += 1;
                    check_state(&reg, &model, &drops);
                    continue;
                };
                let held = receipts[*handle as usize]
                    .take()
                    .expect("model and real receipt handles agree");
                let current = model_epochs.get(&0).copied() == Some(held_model.epoch);
                match settlement {
                    Settlement::Suspended => {
                        let holes = new_holes.iter().copied().map(Hole).collect::<Vec<_>>();
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
                            model_epochs.remove(&0);
                            coverage.live_settlements[2] += 1;
                        } else {
                            coverage.stale_settlements[2] += 1;
                        }
                    }
                }
            }
        }
        check_state(&reg, &model, &drops);
    }

    // A generated prefix can leave receipts outstanding. Explicitly restore
    // each remaining one so the debug receipt guard never becomes cleanup.
    for handle in 0..receipts.len() {
        if let Some(held) = receipts[handle].take() {
            let held_model = model_receipts[handle]
                .take()
                .expect("receipt model survives cleanup");
            let current = model_epochs.get(&0).copied() == Some(held_model.epoch);
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
    coverage
}

fn delayed_receipt_history(stale: Settlement, suffix: &[ReceiptOp]) -> Vec<ReceiptOp> {
    let live = next_settlement(stale);
    let mut ops = vec![
        ReceiptOp::Insert(1, 10),
        ReceiptOp::Detach(0, 2),
        ReceiptOp::Detach(0, 1), // explicit occupied logical handle refusal
        ReceiptOp::Remove(1),
        ReceiptOp::Insert(2, 20),
        ReceiptOp::Settle(0, stale, vec![3, 1, 1], 2),
        ReceiptOp::Insert(3, 30),
        ReceiptOp::Detach(1, -1),
        ReceiptOp::Settle(1, live, vec![2, 2, 0], 3),
        ReceiptOp::Settle(1, live, vec![], 4), // explicit missing logical receipt
    ];
    ops.extend_from_slice(suffix);
    ops
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

    #[test]
    fn generated_tombstone_histories_evict_by_retirement_event(
        extra_events in super::TOMBSTONE_CAPACITY..(super::TOMBSTONE_CAPACITY + 12),
    ) {
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
        prop_assert!(coverage.tombstone_evictions > 0, "{coverage:?}");
        prop_assert!(coverage.checkout_refused[2] > 0, "{coverage:?}");
    }
}
