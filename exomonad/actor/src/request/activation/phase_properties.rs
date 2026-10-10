//! Request-settlement component histories with authentic retained resources.
//! Native claim/outcome/confirmation facts are supplied at the native-owner
//! boundary; actual manifest transactions and async task routing are qualified
//! by the separate native activation cluster. Observations never reconcile.

use super::*;
use crate::request::{
    CancelRequestOutcome, CancellationReason, ForgetResponseOutcome, PendingProgress,
    ReplyObservation, ResponseFailure, ResponseObservation,
};
use crate::resident_workbench::request_tests::ActivationPublicationPhaseFixture;
use crate::{ActorExitKind, ActorId, ActorTerminal, Incarnation};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseError, TestRunner};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Caller {
    Exact,
    Foreign,
    Restarted,
}

impl Caller {
    fn actor(self, exact: ActorRef) -> ActorRef {
        match self {
            Self::Exact => exact,
            Self::Foreign => ActorRef::first(ActorId(exact.id.0 + 1_000)),
            Self::Restarted => ActorRef {
                incarnation: Incarnation(exact.incarnation.0 + 1),
                ..exact
            },
        }
    }

    fn authorize(self, forgotten: bool) -> Result<(), ReplyError> {
        if forgotten {
            return Err(ReplyError::Stale);
        }
        match self {
            Self::Exact => Ok(()),
            Self::Foreign => Err(ReplyError::Unauthorized),
            Self::Restarted => Err(ReplyError::WrongIncarnation),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum NativeOutcome {
    Visible,
    BeforeRename,
}

impl NativeOutcome {
    fn visible(self) -> bool {
        matches!(self, Self::Visible)
    }
}

#[derive(Clone, Copy, Debug)]
enum CapabilityRef {
    ForRequest,
    First,
    Last,
    Slot(u8),
    Missing,
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Claim(bool),
    Finish(NativeOutcome),
    Decide(NativeOutcome),
    FinishToken,
    LoseWaiter,
    DropNativeLease,
    ConfirmOwner,
    CancelNative,
    ShutdownTarget,
    Deliver,
    Publish,
    Reconcile,
    Read,
    BeginReply,
    FinishReply,
    Cancel,
    BeginAck,
    RollbackAck,
    FinishAck,
    RetireTarget,
    RetireOwner,
    Forget,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Claim(_) => "claim",
            Self::Finish(_) => "native_finish",
            Self::Decide(_) => "native_decide",
            Self::FinishToken => "finish_token",
            Self::LoseWaiter => "lose_waiter",
            Self::DropNativeLease => "drop_native_lease",
            Self::ConfirmOwner => "owner_confirm",
            Self::CancelNative => "native_cancel",
            Self::ShutdownTarget => "shutdown_target",
            Self::Deliver => "deliver",
            Self::Publish => "publish",
            Self::Reconcile => "reconcile",
            Self::Read => "read",
            Self::BeginReply => "begin_reply",
            Self::FinishReply => "finish_reply",
            Self::Cancel => "cancel",
            Self::BeginAck => "begin_ack",
            Self::RollbackAck => "rollback_ack",
            Self::FinishAck => "finish_ack",
            Self::RetireTarget => "retire_target",
            Self::RetireOwner => "retire_owner",
            Self::Forget => "forget",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Operation {
    request: usize,
    capability: CapabilityRef,
    caller: Caller,
    action: Action,
}

fn op(request: usize, action: Action) -> Operation {
    Operation {
        request,
        capability: CapabilityRef::ForRequest,
        caller: Caller::Exact,
        action,
    }
}

fn operations() -> impl Strategy<Value = Operation> {
    let outcome = prop_oneof![
        Just(NativeOutcome::Visible),
        Just(NativeOutcome::BeforeRename)
    ];
    let action = prop_oneof![
        6 => any::<bool>().prop_map(Action::Claim),
        4 => outcome.clone().prop_map(Action::Finish),
        3 => outcome.prop_map(Action::Decide), 2 => Just(Action::FinishToken),
        3 => Just(Action::LoseWaiter), 2 => Just(Action::DropNativeLease),
        3 => Just(Action::ConfirmOwner), 3 => Just(Action::CancelNative), 1 => Just(Action::ShutdownTarget), 3 => Just(Action::Deliver),
        4 => Just(Action::Publish), 3 => Just(Action::Reconcile),
        2 => Just(Action::Read), 3 => Just(Action::BeginReply),
        2 => Just(Action::FinishReply), 3 => Just(Action::Cancel),
        3 => Just(Action::BeginAck), 1 => Just(Action::RollbackAck),
        2 => Just(Action::FinishAck), 1 => Just(Action::RetireTarget),
        1 => Just(Action::RetireOwner), 2 => Just(Action::Forget),
    ];
    let reference = prop_oneof![
        6 => Just(CapabilityRef::ForRequest), 1 => Just(CapabilityRef::First),
        1 => Just(CapabilityRef::Last), 2 => (0u8..6).prop_map(CapabilityRef::Slot),
        1 => Just(CapabilityRef::Missing),
    ];
    let caller = prop_oneof![6 => Just(Caller::Exact), 1 => Just(Caller::Foreign), 1 => Just(Caller::Restarted)];
    (0usize..4, reference, caller, action).prop_map(|(request, capability, caller, action)| {
        Operation {
            request,
            capability,
            caller,
            action,
        }
    })
}

fn guided(mode: u8, cancel_native: bool) -> Vec<Operation> {
    use Action::*;
    let claim = op(0, Claim(true));
    let mut core = match mode {
        0 => vec![op(0, Cancel), claim, op(0, BeginAck), op(0, FinishAck)],
        1 => vec![
            claim,
            op(0, BeginReply),
            op(0, Cancel),
            op(0, BeginAck),
            op(0, LoseWaiter),
            op(0, Finish(NativeOutcome::Visible)),
            op(0, Deliver),
            op(0, BeginAck),
            op(0, FinishAck),
            op(0, Forget),
        ],
        2 => vec![
            claim,
            op(0, DropNativeLease),
            op(0, Reconcile),
            op(0, BeginReply),
            op(0, Cancel),
            op(0, BeginAck),
            op(0, ConfirmOwner),
            op(0, Read),
        ],
        3 => vec![
            claim,
            op(0, LoseWaiter),
            op(0, Finish(NativeOutcome::Visible)),
            op(0, Reconcile),
            op(0, BeginReply),
            op(0, ConfirmOwner),
            op(0, Deliver),
            op(0, BeginReply),
            op(0, FinishReply),
        ],
        4 => vec![
            claim,
            op(0, Cancel),
            op(0, BeginAck),
            op(0, Finish(NativeOutcome::BeforeRename)),
            op(0, BeginAck),
            op(0, RollbackAck),
            op(0, BeginAck),
            op(0, FinishAck),
            op(0, Deliver),
            op(0, Publish),
        ],
        5 => vec![
            claim,
            op(0, Finish(NativeOutcome::Visible)),
            op(0, Deliver),
            op(0, Publish),
            op(0, Claim(true)),
            op(0, BeginReply),
            op(0, FinishReply),
            op(0, Forget),
        ],
        6 => vec![
            claim,
            op(0, RetireTarget),
            op(0, Forget),
            op(0, Finish(NativeOutcome::Visible)),
            op(0, ConfirmOwner),
            op(0, Deliver),
            op(0, Publish),
            op(0, Reconcile),
            op(0, Read),
        ],
        7 => vec![
            claim,
            op(0, RetireOwner),
            op(0, Finish(NativeOutcome::Visible)),
            op(0, Deliver),
            op(0, Publish),
            op(0, BeginReply),
            op(0, FinishReply),
            op(0, Forget),
        ],
        8 => vec![
            op(0, Claim(false)),
            claim,
            op(0, Claim(true)),
            op(0, Finish(NativeOutcome::BeforeRename)),
            op(0, Deliver),
            op(0, Publish),
            op(0, BeginReply),
        ],
        9 => vec![
            claim,
            op(0, Finish(NativeOutcome::BeforeRename)),
            op(0, Claim(true)),
            op(0, Deliver),
            op(0, Publish),
            op(0, BeginReply),
        ],
        10 => vec![
            claim,
            op(1, Claim(true)),
            op(0, RetireTarget),
            op(1, LoseWaiter),
            op(1, Finish(NativeOutcome::Visible)),
            op(1, Deliver),
            op(1, Publish),
            op(0, DropNativeLease),
            op(0, Forget),
            op(1, Forget),
        ],
        11 => {
            let mut core = vec![];
            for caller in [Caller::Foreign, Caller::Restarted] {
                for action in [Claim(true), Cancel, BeginReply, BeginAck, Forget] {
                    core.push(Operation {
                        caller,
                        ..op(0, action)
                    });
                }
            }
            core.extend([
                claim,
                op(0, Finish(NativeOutcome::Visible)),
                op(0, Cancel),
                op(0, BeginAck),
                op(0, ConfirmOwner),
                op(0, BeginAck),
                op(0, FinishAck),
                op(0, Deliver),
                op(0, Publish),
            ]);
            core
        }
        12 => vec![
            claim,
            op(0, LoseWaiter),
            op(0, Decide(NativeOutcome::Visible)),
            op(0, BeginReply),
            op(0, FinishToken),
            op(0, Deliver),
            op(0, FinishReply),
            op(0, Forget),
        ],
        13 => vec![
            claim,
            op(0, LoseWaiter),
            op(0, Decide(NativeOutcome::Visible)),
            op(0, Cancel),
            op(0, BeginAck),
            op(0, ConfirmOwner),
            op(0, BeginAck),
            op(0, FinishAck),
            op(0, DropNativeLease),
        ],
        14 => vec![
            op(0, ShutdownTarget),
            claim,
            op(0, BeginReply),
            op(0, FinishReply),
        ],
        15 => vec![
            op(2, Claim(true)),
            claim,
            op(0, FinishToken),
            op(0, Deliver),
            op(0, Publish),
            op(0, Decide(NativeOutcome::BeforeRename)),
            op(0, Reconcile),
            op(0, BeginReply),
        ],
        16 => vec![
            claim,
            op(1, Claim(true)),
            op(0, DropNativeLease),
            op(1, Decide(NativeOutcome::Visible)),
            op(0, ConfirmOwner),
            op(0, Reconcile),
            op(1, Reconcile),
            op(0, BeginReply),
            op(1, BeginReply),
            op(1, FinishToken),
            op(1, Deliver),
        ],
        _ => unreachable!("bounded guided mode"),
    };
    if cancel_native {
        if let Some(index) = core
            .iter()
            .position(|operation| matches!(operation.action, Claim(true)))
        {
            core.insert(index + 1, op(0, CancelNative));
        }
    }
    core
}

fn histories() -> impl Strategy<Value = Vec<Operation>> {
    let arbitrary = proptest::collection::vec(operations(), 1..65);
    let guided = (0u8..17, any::<bool>()).prop_flat_map(|(mode, cancel_native)| {
        let core = guided(mode, cancel_native);
        (
            proptest::collection::vec(operations(), 0..8),
            proptest::collection::vec(proptest::bool::weighted(1.0), core.len()),
            proptest::collection::vec(operations(), 0..12),
        )
            .prop_map(move |(prefix, keep, suffix)| {
                let mut history: Vec<_> = prefix
                    .into_iter()
                    .map(|mut operation| {
                        operation.request = 3;
                        operation.capability = CapabilityRef::ForRequest;
                        if matches!(
                            operation.action,
                            Action::RetireOwner | Action::ConfirmOwner | Action::ShutdownTarget
                        ) {
                            operation.action = Action::Read;
                        }
                        operation
                    })
                    .collect();
                history.extend(
                    core.iter()
                        .zip(keep)
                        .filter_map(|(op, keep)| keep.then_some(*op)),
                );
                history.extend(suffix);
                history
            })
    });
    prop_oneof![1 => arbitrary, 2 => guided]
}

#[derive(Clone, Copy, Debug)]
enum NativeFact {
    Claimed,
    LeaseLost,
    CancellationRequested,
    Decided(NativeOutcome),
    Reconciled { owner_ready: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpectedNative {
    NotClaimed,
    InFlight,
    Unknown,
    VisibleUnconfirmed,
    VisibleConfirmed,
    Refused,
}

impl ExpectedNative {
    fn fenced(self) -> bool {
        matches!(
            self,
            Self::InFlight | Self::Unknown | Self::VisibleUnconfirmed
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpectedCache {
    Absent,
    Retained,
    Published,
    Refused,
}

#[derive(Debug, Default)]
struct NativeFacts {
    accepted: Vec<NativeFact>,
}

impl NativeFacts {
    fn decision(&self) -> Option<(usize, NativeOutcome)> {
        self.accepted.iter().enumerate().find_map(|(index, fact)| {
            if let NativeFact::Decided(outcome) = fact {
                Some((index, *outcome))
            } else {
                None
            }
        })
    }

    // Accepted native outcomes are independent of waiter/token lifetime and
    // registry cache refresh. Owner readiness alone cannot supply visibility.
    fn observed(&self, owner_ready: bool) -> ExpectedNative {
        if !self
            .accepted
            .iter()
            .any(|fact| matches!(fact, NativeFact::Claimed))
        {
            return ExpectedNative::NotClaimed;
        }
        match self.decision() {
            Some((_, outcome)) if outcome.visible() => {
                if owner_ready {
                    ExpectedNative::VisibleConfirmed
                } else {
                    ExpectedNative::VisibleUnconfirmed
                }
            }
            Some(_) => ExpectedNative::Refused,
            None if self
                .accepted
                .iter()
                .any(|fact| matches!(fact, NativeFact::LeaseLost)) =>
            {
                ExpectedNative::Unknown
            }
            None => ExpectedNative::InFlight,
        }
    }

    // Full scan of reconcile facts after the native outcome models deferred
    // resource release separately from authoritative publication evidence.
    fn cache(&self) -> ExpectedCache {
        if !self
            .accepted
            .iter()
            .any(|fact| matches!(fact, NativeFact::Claimed))
        {
            return ExpectedCache::Absent;
        }
        if let Some((index, outcome)) = self.decision() {
            let terminal = self.accepted[index + 1..].iter().any(|fact| match fact {
                NativeFact::Reconciled { owner_ready } => !outcome.visible() || *owner_ready,
                _ => false,
            });
            if terminal {
                return if outcome.visible() {
                    ExpectedCache::Published
                } else {
                    ExpectedCache::Refused
                };
            }
        }
        ExpectedCache::Retained
    }

    fn reconcile(&mut self, owner_ready: bool) {
        self.accepted.push(NativeFact::Reconciled { owner_ready });
    }
}

#[derive(Debug)]
enum RequestFact {
    Cancelled,
    ReplyClaimed,
    ReplyFinished,
    AckClaimed,
    AckRolledBack,
    AckFinished,
    TargetRetired,
    OwnerRetired,
    Forgotten,
}

#[derive(Debug, Default)]
struct RequestFacts {
    accepted: Vec<RequestFact>,
    native: NativeFacts,
}

impl RequestFacts {
    fn forgotten(&self) -> bool {
        self.accepted
            .iter()
            .any(|fact| matches!(fact, RequestFact::Forgotten))
    }

    fn closed(&self) -> bool {
        self.accepted.iter().any(|fact| {
            matches!(
                fact,
                RequestFact::ReplyFinished | RequestFact::AckFinished | RequestFact::TargetRetired
            )
        })
    }

    fn cancelled(&self) -> bool {
        self.accepted
            .iter()
            .any(|fact| matches!(fact, RequestFact::Cancelled))
    }

    fn reply_claimed(&self) -> bool {
        self.accepted
            .iter()
            .any(|fact| matches!(fact, RequestFact::ReplyClaimed))
    }

    fn ack_claimed(&self) -> bool {
        self.accepted
            .iter()
            .rev()
            .find_map(|fact| match fact {
                RequestFact::AckClaimed => Some(true),
                RequestFact::AckRolledBack => Some(false),
                _ => None,
            })
            .unwrap_or(false)
    }

    fn first_outcome(&self) -> Option<Result<(), ResponseFailure>> {
        self.accepted.iter().find_map(|fact| match fact {
            RequestFact::ReplyFinished => Some(Ok(())),
            RequestFact::AckFinished => Some(Err(ResponseFailure::Cancelled)),
            RequestFact::TargetRetired => Some(Err(ResponseFailure::TargetUnavailable)),
            RequestFact::OwnerRetired => Some(Err(ResponseFailure::RequesterStopped)),
            _ => None,
        })
    }

    fn target_eligibility(&self) -> Result<(), ReplyError> {
        if self.closed() {
            Err(ReplyError::AlreadySettled)
        } else if self.cancelled() {
            Err(ReplyError::CancellationRequested)
        } else if self.reply_claimed() {
            Err(ReplyError::Stale)
        } else {
            Ok(())
        }
    }

    fn response(&self) -> Result<ResponseObservation, ReplyError> {
        if self.forgotten() {
            return Err(ReplyError::Stale);
        }
        Ok(match self.first_outcome() {
            Some(Ok(())) => ResponseObservation::Ready,
            Some(Err(failure)) => ResponseObservation::Unavailable(failure),
            None if self.cancelled() && !self.ack_claimed() => {
                ResponseObservation::CancellationPending(CancellationReason::RequesterCancelled)
            }
            None => ResponseObservation::Pending(PendingProgress {
                actor_terminal: None,
                provider_turn: None,
                last_activity_unix_ms: None,
                progress_revision: None,
                watched: false,
            }),
        })
    }

    fn reply(&self) -> Result<ReplyObservation, ReplyError> {
        if self.forgotten() {
            return Err(ReplyError::Stale);
        }
        Ok(if self.closed() {
            ReplyObservation::Closed
        } else if self.cancelled() {
            ReplyObservation::CancellationRequested(CancellationReason::RequesterCancelled)
        } else {
            ReplyObservation::Open
        })
    }
}

struct Capability {
    request: usize,
    lease: Option<RequestActivationPublication>,
    completion: Option<RequestActivationCompletion>,
    received: Option<RequestActivationCompletion>,
    waiter_lost: bool,
    claim: Option<PublicationClaim>,
    resources: Arc<crate::resident_workbench::ActivationPublicationResources>,
    decision: Arc<tidepool_runtime::session::PublicationDecision>,
    waiter: Option<tokio::sync::oneshot::Receiver<RequestActivationCompletion>>,
    sender: Option<tokio::sync::oneshot::Sender<RequestActivationCompletion>>,
}

fn resolve(reference: CapabilityRef, request: usize, capabilities: &[Capability]) -> Option<usize> {
    match reference {
        CapabilityRef::ForRequest => capabilities.iter().rposition(|cap| cap.request == request),
        CapabilityRef::First => (!capabilities.is_empty()).then_some(0),
        CapabilityRef::Last => capabilities.len().checked_sub(1),
        CapabilityRef::Slot(slot) => {
            (usize::from(slot) < capabilities.len()).then_some(usize::from(slot))
        }
        CapabilityRef::Missing => None,
    }
}

#[derive(Debug, Default)]
struct Coverage {
    operations: BTreeMap<&'static str, usize>,
    refusals: BTreeMap<&'static str, usize>,
    logical_refusals: BTreeMap<&'static str, usize>,
    claims: usize,
    claim_control_refusals: usize,
    lost_waiters: usize,
    finishes_after_lost_waiter: usize,
    confirmations_after_lost_waiter: usize,
    deliveries_to_lost_waiter: usize,
    unknown_with_ready_owner: usize,
    known_unconfirmed_with_pending_owner: usize,
    fenced_replies: usize,
    fenced_acknowledgements: usize,
    native_finishes: BTreeMap<&'static str, usize>,
    native_confirmations: usize,
    native_cancellations: usize,
    releases_without_token_finish: usize,
    retirement_claim_refusals: usize,
    owner_authority_refusals: usize,
    provider_publications: usize,
    suppressed_provider_publications: usize,
    forgotten_with_native_lease: usize,
    completion_after_forgetting: usize,
    shared_target_retirements: usize,
}

fn check<T: std::fmt::Debug + PartialEq>(
    actual: Result<T, ReplyError>,
    expected: Result<T, ReplyError>,
    action: Action,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    if expected.is_err() {
        *coverage.refusals.entry(action.name()).or_default() += 1;
    }
    prop_assert_eq!(actual, expected);
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum ExpectedClaimRefusal {
    Request(ReplyError),
    Retired,
}

fn run_history(
    operations: &[Operation],
    fixture: &ActivationPublicationPhaseFixture,
    initially_pending: bool,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    // Reset actual native confirmation state for each replay/shrink. Compilation
    // and authentic input admission happened once at family setup, not here.
    fixture.make_ready();
    if initially_pending {
        fixture.make_pending();
    }
    let mut owner_ready = !initially_pending;
    let registry = Arc::new(RequestRegistry::default());
    let target = fixture.actor();
    let owner = ActorRef::first(ActorId(target.id.0 + 100));
    let targets = [
        target,
        target,
        ActorRef::first(ActorId(target.id.0 + 200)),
        ActorRef::first(ActorId(target.id.0 + 300)),
    ];
    let retirements: [crate::RetainedActorExit; 4] =
        std::array::from_fn(|_| crate::RetainedActorExit::new());
    let mut shutdown = [false; 4];
    let ids = targets.map(|target| {
        let request = registry.reserve_native(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        request
    });
    let mut facts: [RequestFacts; 4] = std::array::from_fn(|_| RequestFacts::default());
    let mut reply_claims: [Option<crate::request::RequestReplyClaim>; 4] =
        std::array::from_fn(|_| None);
    let mut capabilities: Vec<Capability> = vec![];
    for (step, operation) in operations.iter().enumerate() {
        let Operation {
            request: key,
            capability: reference,
            caller,
            action,
        } = *operation;
        *coverage.operations.entry(action.name()).or_default() += 1;
        let selected = resolve(reference, key, &capabilities);
        match action {
            Action::Claim(permit) => {
                let model = &mut facts[key];
                let expected = caller
                    .authorize(model.forgotten())
                    .map_err(ExpectedClaimRefusal::Request)
                    .and_then(|()| {
                        if targets[key] != target {
                            return Err(ExpectedClaimRefusal::Request(ReplyError::Unauthorized));
                        }
                        model
                            .target_eligibility()
                            .map_err(ExpectedClaimRefusal::Request)?;
                        if model.native.observed(owner_ready) != ExpectedNative::NotClaimed {
                            return Err(ExpectedClaimRefusal::Request(ReplyError::AlreadySettled));
                        }
                        if shutdown[key] {
                            Err(ExpectedClaimRefusal::Retired)
                        } else {
                            Ok(permit)
                        }
                    });
                let (resources, decision) = fixture.fresh_resources();
                prop_assert_eq!(resources.confirmed(), owner_ready);
                if !permit {
                    decision.request_cancellation();
                }
                let actual = registry.begin_activation_publication(
                    caller.actor(targets[key]),
                    ids[key],
                    resources.clone(),
                    &retirements[key],
                );
                let actual_result = actual
                    .as_ref()
                    .map(|claim| claim.is_some())
                    .map_err(|error| match error {
                        ActivationPublicationRefusal::Request(error) => {
                            ExpectedClaimRefusal::Request(*error)
                        }
                        ActivationPublicationRefusal::Retired(_) => ExpectedClaimRefusal::Retired,
                    });
                if let Err(ActivationPublicationRefusal::Retired(terminal)) = &actual {
                    prop_assert_eq!(terminal.kind, ActorExitKind::Completed);
                }
                if expected.is_err() {
                    *coverage.refusals.entry(action.name()).or_default() += 1;
                }
                prop_assert_eq!(&actual_result, &expected);
                coverage.retirement_claim_refusals +=
                    usize::from(expected == Err(ExpectedClaimRefusal::Retired));
                coverage.owner_authority_refusals += usize::from(
                    targets[key] != target
                        && expected == Err(ExpectedClaimRefusal::Request(ReplyError::Unauthorized)),
                );
                // Refusal must not claim the native decision or install metadata.
                prop_assert_eq!(
                    decision.phase(),
                    if actual_result == Ok(true) {
                        PublicationPhase::CommitClaimed {
                            cancellation_pending: false,
                        }
                    } else if permit {
                        PublicationPhase::Running
                    } else {
                        PublicationPhase::CancellationRequested
                    }
                );
                if let Ok(Some((claim, lease))) = actual {
                    model.native.accepted.push(NativeFact::Claimed);
                    let (sender, waiter) = tokio::sync::oneshot::channel();
                    capabilities.push(Capability {
                        request: key,
                        lease: Some(lease),
                        completion: None,
                        received: None,
                        waiter_lost: false,
                        claim: Some(claim),
                        resources,
                        decision,
                        waiter: Some(waiter),
                        sender: Some(sender),
                    });
                    coverage.claims += 1;
                } else if actual_result == Ok(false) {
                    coverage.claim_control_refusals += 1;
                }
            }
            Action::Finish(outcome) | Action::Decide(outcome) => {
                if let Some(cap) = selected
                    .and_then(|index| capabilities.get_mut(index))
                    .filter(|cap| cap.claim.is_some())
                {
                    let model = &mut facts[cap.request];
                    let claim = cap.claim.take().unwrap();
                    if outcome.visible() {
                        claim.published();
                    } else {
                        claim.before_rename_failure();
                    }
                    model.native.accepted.push(NativeFact::Decided(outcome));
                    if matches!(action, Action::Finish(_)) {
                        if let Some(lease) = cap.lease.take() {
                            cap.completion = Some(lease.finish());
                            model.native.reconcile(owner_ready);
                        }
                    }
                    coverage.finishes_after_lost_waiter += usize::from(cap.waiter_lost);
                    coverage.completion_after_forgetting += usize::from(model.forgotten());
                    let partition = match outcome {
                        NativeOutcome::Visible if owner_ready => "visible_confirmed",
                        NativeOutcome::Visible => "visible_unconfirmed",
                        NativeOutcome::BeforeRename => "before_rename",
                    };
                    *coverage.native_finishes.entry(partition).or_default() += 1;
                } else {
                    *coverage.logical_refusals.entry(action.name()).or_default() += 1;
                }
            }
            Action::FinishToken => {
                if let Some(cap) = selected
                    .and_then(|index| capabilities.get_mut(index))
                    .filter(|cap| cap.lease.is_some())
                {
                    cap.completion = Some(cap.lease.take().unwrap().finish());
                    facts[cap.request].native.reconcile(owner_ready);
                } else {
                    *coverage.logical_refusals.entry(action.name()).or_default() += 1;
                }
            }
            Action::DropNativeLease => {
                if let Some(cap) = selected
                    .and_then(|index| capabilities.get_mut(index))
                    .filter(|cap| cap.lease.is_some() || cap.claim.is_some())
                {
                    if cap.lease.is_some() {
                        drop(cap.lease.take());
                        facts[cap.request].native.reconcile(owner_ready);
                    }
                    drop(cap.claim.take());
                    facts[cap.request]
                        .native
                        .accepted
                        .push(NativeFact::LeaseLost);
                } else {
                    *coverage.logical_refusals.entry(action.name()).or_default() += 1;
                }
            }
            Action::LoseWaiter => {
                if let Some(cap) = selected
                    .and_then(|index| capabilities.get_mut(index))
                    .filter(|cap| cap.waiter.is_some())
                {
                    drop(cap.waiter.take());
                    cap.waiter_lost = true;
                    coverage.lost_waiters += 1;
                } else {
                    *coverage.logical_refusals.entry(action.name()).or_default() += 1;
                }
            }
            Action::CancelNative => {
                if let Some(cap) = selected.and_then(|index| capabilities.get_mut(index)) {
                    use tidepool_runtime::session::PublicationCancellation;
                    let model = &mut facts[cap.request];
                    let expected = match model.native.decision() {
                        Some((_, NativeOutcome::Visible)) => {
                            PublicationCancellation::AlreadyPublished
                        }
                        Some((_, NativeOutcome::BeforeRename)) => {
                            PublicationCancellation::AlreadyTerminated
                        }
                        None => PublicationCancellation::PendingCommitOutcome,
                    };
                    prop_assert_eq!(cap.decision.request_cancellation(), expected);
                    coverage.native_cancellations += 1;
                    if expected == PublicationCancellation::PendingCommitOutcome {
                        model
                            .native
                            .accepted
                            .push(NativeFact::CancellationRequested);
                    }
                } else {
                    *coverage.logical_refusals.entry(action.name()).or_default() += 1;
                }
            }
            Action::ConfirmOwner => {
                fixture.make_ready();
                owner_ready = true;
                coverage.native_confirmations += 1;
                coverage.confirmations_after_lost_waiter +=
                    usize::from(capabilities.iter().any(|cap| cap.waiter_lost));
            }
            Action::ShutdownTarget => {
                if caller == Caller::Exact {
                    for (index, _) in targets
                        .iter()
                        .enumerate()
                        .filter(|(_, actor)| **actor == targets[key])
                    {
                        retirements[index].request_shutdown(ActorTerminal {
                            kind: ActorExitKind::Completed,
                            summary: "shutdown".into(),
                            diagnostic: None,
                        });
                        shutdown[index] = true;
                    }
                }
            }
            Action::Deliver => {
                if let Some(cap) = selected
                    .and_then(|index| capabilities.get_mut(index))
                    .filter(|cap| cap.sender.is_some() && cap.completion.is_some())
                {
                    let sent = cap
                        .sender
                        .take()
                        .unwrap()
                        .send(cap.completion.take().unwrap());
                    prop_assert_eq!(sent.is_ok(), cap.waiter.is_some());
                    match sent {
                        Ok(()) => {
                            cap.received = Some(cap.waiter.as_mut().unwrap().try_recv().unwrap());
                            drop(cap.waiter.take());
                        }
                        Err(completion) => {
                            drop(completion);
                            facts[cap.request].native.reconcile(owner_ready);
                            coverage.deliveries_to_lost_waiter += 1;
                        }
                    }
                } else {
                    *coverage.logical_refusals.entry(action.name()).or_default() += 1;
                }
            }
            Action::Publish => {
                if let Some(cap) = selected
                    .and_then(|index| capabilities.get_mut(index))
                    .filter(|cap| cap.received.is_some())
                {
                    let model = &mut facts[cap.request];
                    if !model.forgotten() {
                        model.native.reconcile(owner_ready);
                    }
                    let expected = if model.forgotten() {
                        Err(ReplyError::Stale)
                    } else if model.native.observed(owner_ready) != ExpectedNative::VisibleConfirmed
                    {
                        Err(ReplyError::Stale)
                    } else {
                        model.target_eligibility()
                    };
                    let mut invoked = false;
                    let actual = cap.received.take().unwrap().publish_if_current(|| {
                        invoked = true;
                    });
                    // The consuming token's destructor also reconciles, including
                    // refused publication. It cannot turn an unknown claim into proof.
                    model.native.reconcile(owner_ready);
                    prop_assert_eq!(invoked, expected.is_ok());
                    coverage.provider_publications += usize::from(expected.is_ok());
                    coverage.suppressed_provider_publications += usize::from(expected.is_err());
                    check(actual, expected, action, coverage)?;
                } else {
                    *coverage.logical_refusals.entry(action.name()).or_default() += 1;
                }
            }
            Action::Read => {}
            Action::Reconcile => {
                let mut state = registry.state.lock();
                if let Some(record) = state.requests.get_mut(&ids[key]) {
                    if let Some(activation) = record.activation.as_mut() {
                        activation.reconcile();
                        facts[key].native.reconcile(owner_ready);
                    }
                }
            }
            Action::BeginReply => {
                let model = &mut facts[key];
                let expected = caller.authorize(model.forgotten()).and_then(|()| {
                    model.target_eligibility()?;
                    model.native.reconcile(owner_ready);
                    if model.native.observed(owner_ready).fenced() {
                        Err(ReplyError::UpdatePending)
                    } else {
                        Ok(())
                    }
                });
                coverage.fenced_replies += usize::from(expected == Err(ReplyError::UpdatePending));
                if expected.is_ok()
                    && capabilities
                        .iter()
                        .any(|cap| cap.request == key && cap.lease.is_some())
                {
                    coverage.releases_without_token_finish += 1;
                }
                check(
                    registry
                        .begin_reply(caller.actor(targets[key]), ids[key])
                        .map(|claim| {
                            reply_claims[key] = Some(claim);
                        }),
                    expected,
                    action,
                    coverage,
                )?;
                if expected.is_ok() {
                    model.accepted.push(RequestFact::ReplyClaimed);
                }
            }
            Action::FinishReply => {
                let model = &mut facts[key];
                if !model.forgotten() && !model.closed() && model.reply_claimed() {
                    model.accepted.push(RequestFact::ReplyFinished);
                }
                prop_assert!(crate::request::test_support::complete_optional_reply(
                    &registry,
                    &mut reply_claims[key],
                    None
                )
                .is_empty());
            }
            Action::Cancel => {
                let model = &mut facts[key];
                let expected = caller.authorize(model.forgotten()).map(|()| {
                    if model.closed() || model.reply_claimed() {
                        CancelRequestOutcome::AlreadyTerminal
                    } else if model.cancelled() {
                        CancelRequestOutcome::AlreadyRequested
                    } else {
                        CancelRequestOutcome::Requested
                    }
                });
                let actual = registry.cancel_request(
                    caller.actor(owner),
                    ids[key],
                    CancellationReason::RequesterCancelled,
                );
                if let Ok((outcome, notice)) = &actual {
                    prop_assert_eq!(
                        notice.is_some(),
                        *outcome == CancelRequestOutcome::Requested
                    );
                }
                check(
                    actual.map(|(outcome, _)| outcome),
                    expected,
                    action,
                    coverage,
                )?;
                if expected == Ok(CancelRequestOutcome::Requested) {
                    model.accepted.push(RequestFact::Cancelled);
                }
            }
            Action::BeginAck => {
                let model = &mut facts[key];
                let expected = caller.authorize(model.forgotten()).and_then(|()| {
                    if model.closed() {
                        return Err(ReplyError::AlreadySettled);
                    }
                    if !model.cancelled() || model.ack_claimed() {
                        return Err(ReplyError::Stale);
                    }
                    model.native.reconcile(owner_ready);
                    if model.native.observed(owner_ready).fenced() {
                        Err(ReplyError::UpdatePending)
                    } else {
                        Ok(CancellationReason::RequesterCancelled)
                    }
                });
                coverage.fenced_acknowledgements +=
                    usize::from(expected == Err(ReplyError::UpdatePending));
                check(
                    registry
                        .begin_cancellation_acknowledgement(caller.actor(targets[key]), ids[key]),
                    expected,
                    action,
                    coverage,
                )?;
                if expected.is_ok() {
                    model.accepted.push(RequestFact::AckClaimed);
                }
            }
            Action::RollbackAck => {
                let model = &mut facts[key];
                if !model.forgotten() && !model.closed() && model.ack_claimed() {
                    model.accepted.push(RequestFact::AckRolledBack);
                }
                registry.rollback_cancellation_acknowledgement(ids[key]);
            }
            Action::FinishAck => {
                let model = &mut facts[key];
                if !model.forgotten() && !model.closed() && model.ack_claimed() {
                    model.accepted.push(RequestFact::AckFinished);
                }
                prop_assert!(registry
                    .finish_cancellation_acknowledgement(ids[key])
                    .is_empty());
            }
            Action::RetireTarget | Action::RetireOwner => {
                let actor = caller.actor(if matches!(action, Action::RetireTarget) {
                    targets[key]
                } else {
                    owner
                });
                let mut retired = 0;
                for (key, model) in facts
                    .iter_mut()
                    .enumerate()
                    .filter(|(_, model)| !model.forgotten())
                {
                    if targets[key] == actor && !model.closed() {
                        model.accepted.push(RequestFact::TargetRetired);
                        retired += 1;
                    } else if owner == actor && model.first_outcome().is_none() {
                        model.accepted.push(RequestFact::OwnerRetired);
                    }
                }
                coverage.shared_target_retirements += usize::from(retired > 1);
                let terminal = ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "retired".into(),
                    diagnostic: None,
                };
                prop_assert!(registry.actor_stopped(actor, &terminal).is_empty());
            }
            Action::Forget => {
                let model = &mut facts[key];
                let expected = caller.authorize(model.forgotten()).map(|()| {
                    if model.first_outcome().is_none() {
                        ForgetResponseOutcome::StillPending
                    } else if !model.closed() {
                        ForgetResponseOutcome::TargetStillActive
                    } else {
                        ForgetResponseOutcome::Forgotten
                    }
                });
                let forgotten = expected == Ok(ForgetResponseOutcome::Forgotten);
                check(
                    registry.forget_response(caller.actor(owner), ids[key]).map(
                        |(outcome, notices)| {
                            assert!(notices.is_empty());
                            outcome
                        },
                    ),
                    expected,
                    action,
                    coverage,
                )?;
                if forgotten {
                    coverage.forgotten_with_native_lease += usize::from(
                        capabilities
                            .iter()
                            .any(|cap| cap.request == key && cap.lease.is_some()),
                    );
                    model.accepted.push(RequestFact::Forgotten);
                }
            }
        }
        // Pure observations: do not invoke reconciliation or custody mutation.
        for cap in &capabilities {
            let model = &facts[cap.request].native;
            let expected = match model.decision() {
                Some((_, NativeOutcome::Visible)) => PublicationPhase::Published,
                Some((_, NativeOutcome::BeforeRename)) => PublicationPhase::Terminated,
                None => PublicationPhase::CommitClaimed {
                    cancellation_pending: model
                        .accepted
                        .iter()
                        .any(|fact| matches!(fact, NativeFact::CancellationRequested)),
                },
            };
            prop_assert_eq!(
                cap.resources.phase(),
                expected,
                "step {}: {:?}",
                step,
                operation
            );
            prop_assert_eq!(cap.resources.confirmed(), owner_ready);
        }
        for (key, model) in facts.iter().enumerate() {
            let state = registry.state.lock();
            if model.forgotten() {
                prop_assert!(!state.requests.contains_key(&ids[key]));
            } else {
                let record = state.requests.get(&ids[key]).unwrap();
                let expected = model.native.observed(owner_ready);
                let actual_cache =
                    record
                        .activation
                        .as_ref()
                        .map_or(ExpectedCache::Absent, |activation| match activation {
                            ActivationRecord::Pending(_) => ExpectedCache::Retained,
                            ActivationRecord::Published => ExpectedCache::Published,
                            ActivationRecord::Refused => ExpectedCache::Refused,
                        });
                prop_assert_eq!(
                    actual_cache,
                    model.native.cache(),
                    "step {}: {:?}, request {}",
                    step,
                    operation,
                    key
                );
                if let Some(ActivationRecord::Pending(retained)) = &record.activation {
                    let cap = capabilities.iter().find(|cap| cap.request == key).unwrap();
                    prop_assert!(Arc::ptr_eq(retained, &cap.resources));
                    prop_assert_eq!(retained.confirmed(), owner_ready);
                    let actual = match retained.phase() {
                        PublicationPhase::CommitClaimed { .. }
                            if expected == ExpectedNative::Unknown =>
                        {
                            ExpectedNative::Unknown
                        }
                        PublicationPhase::CommitClaimed { .. } => ExpectedNative::InFlight,
                        PublicationPhase::Published if owner_ready => {
                            ExpectedNative::VisibleConfirmed
                        }
                        PublicationPhase::Published => ExpectedNative::VisibleUnconfirmed,
                        PublicationPhase::Terminated => ExpectedNative::Refused,
                        other => {
                            return Err(TestCaseError::fail(format!(
                                "invalid admitted native phase: {other:?}"
                            )))
                        }
                    };
                    prop_assert_eq!(
                        actual,
                        expected,
                        "step {}: {:?}, request {}",
                        step,
                        operation,
                        key
                    );
                }
                coverage.unknown_with_ready_owner +=
                    usize::from(owner_ready && expected == ExpectedNative::Unknown);
                coverage.known_unconfirmed_with_pending_owner +=
                    usize::from(!owner_ready && expected == ExpectedNative::VisibleUnconfirmed);
            }
            drop(state);
            for caller in [Caller::Exact, Caller::Foreign, Caller::Restarted] {
                prop_assert_eq!(
                    registry.observe_response(caller.actor(owner), ids[key]),
                    model.response(),
                    "step {}: {:?}, request {}",
                    step,
                    operation,
                    key
                );
                prop_assert_eq!(
                    registry.observe_reply(caller.actor(targets[key]), ids[key]),
                    model.reply(),
                    "step {}: {:?}, request {}",
                    step,
                    operation,
                    key
                );
            }
        }
    }
    Ok(())
}

fn config(name: &'static str) -> Config {
    let mut config = Config {
        source_file: Some(file!()),
        test_name: Some(name),
        ..Config::default()
    };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4_096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        eprintln!("activation phase seed persistence: {path}");
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(name);
    config
}

#[tokio::test]
async fn generated_request_activation_matches_native_evidence() {
    let fixture =
        crate::resident_workbench::request_tests::activation_publication_phase_fixture().await;
    for initially_pending in [false, true] {
        let coverage = RefCell::new(Coverage::default());
        let result = TestRunner::new(config(concat!(
            module_path!(),
            "::generated_request_activation_matches_native_evidence"
        )))
        .run(&histories(), |history| {
            run_history(
                &history,
                &fixture,
                initially_pending,
                &mut coverage.borrow_mut(),
            )
        });
        eprintln!(
            "activation phase initially_pending={initially_pending} generated coverage: {:#?}",
            coverage.borrow()
        );
        result.unwrap();
    }
}

#[tokio::test]
async fn request_activation_phase_support_covers_contract_partitions() {
    let fixture =
        crate::resident_workbench::request_tests::activation_publication_phase_fixture().await;
    let mut coverage = Coverage::default();
    for initially_pending in [false, true] {
        for mode in 0..17 {
            for cancel_native in [false, true] {
                run_history(
                    &guided(mode, cancel_native),
                    &fixture,
                    initially_pending,
                    &mut coverage,
                )
                .unwrap();
            }
        }
    }
    assert!(coverage.claims > 0 && coverage.claim_control_refusals > 0);
    assert!(coverage.lost_waiters > 0 && coverage.finishes_after_lost_waiter > 0);
    assert!(coverage.confirmations_after_lost_waiter > 0 && coverage.deliveries_to_lost_waiter > 0);
    assert!(
        coverage.unknown_with_ready_owner > 0 && coverage.known_unconfirmed_with_pending_owner > 0
    );
    assert!(coverage.fenced_replies > 0 && coverage.fenced_acknowledgements > 0);
    assert!(coverage.native_confirmations > 0 && coverage.provider_publications > 0);
    assert!(coverage.native_cancellations > 0);
    assert!(
        coverage.suppressed_provider_publications > 0 && coverage.shared_target_retirements > 0
    );
    assert!(coverage.forgotten_with_native_lease > 0 && coverage.completion_after_forgetting > 0);
    assert!(coverage.releases_without_token_finish > 0 && coverage.retirement_claim_refusals > 0);
    assert!(coverage.owner_authority_refusals > 0);
    for partition in ["visible_confirmed", "visible_unconfirmed", "before_rename"] {
        assert!(
            coverage
                .native_finishes
                .get(partition)
                .copied()
                .unwrap_or_default()
                > 0,
            "{partition}"
        );
    }
    for action in ["claim", "begin_reply", "begin_ack", "publish"] {
        assert!(
            coverage.refusals.get(action).copied().unwrap_or_default() > 0,
            "{action}"
        );
    }
    eprintln!("activation phase deterministic coverage: {coverage:#?}");
}
