//! Real GHC -> Core -> resident JIT proof for typed actor exit retention.
//!
//! A Haskell `ActorRef` publishes a closure into its managed cell, parks on
//! Rust's exact-incarnation wait registry, then observes that same closure on
//! two independent waits after the target has exited.

use tidepool_actor::{
    actor_terminal_value, mount_actor_turn, ActorDescriptor, ActorExitKind, ActorPlacement,
    ActorRegistry, ActorTerminal, ActorTurnKind, ActorWait, StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::Value;
use tidepool_runtime::session::{OutputSink, ResidentOutcome, ResidentSession};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness::{self, mock, EvalHarness};

mod support;

#[derive(Clone, Default)]
struct TestSink;

impl OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }

    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

struct AsSink<Handlers>(Handlers);

impl<Handlers: DispatchEffect<()>> DispatchEffect<TestSink> for AsSink<Handlers> {
    fn dispatch(
        &mut self,
        request: &Value,
        context: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        let unit_context = EffectContext::with_user(context.table(), &());
        self.0.dispatch(request, &unit_context)
    }
}

fn ready_actor(
    registry: &ActorRegistry,
    owner: Option<tidepool_actor::ActorRef>,
    label: &str,
    realm: u64,
) -> tidepool_actor::ActorRef {
    let starting = registry
        .begin_start(
            owner,
            ActorDescriptor::new(
                label,
                ["Actor"],
                ActorPlacement {
                    session: support::process_unique_session(1),
                    resource_scope: RealmId(realm),
                    lexical_scope: ScopeId::ROOT,
                },
            ),
            StartInitiator::Runtime,
        )
        .expect("begin actor");
    registry.publish_ready(starting).expect("publish actor")
}

#[test]
fn typed_wait_retains_completion_and_reports_failure_and_cancellation() {
    eval_harness::require_extract();

    let registry = ActorRegistry::new();
    let waiter = ready_actor(&registry, None, "waiter", 11);
    let target = ready_actor(&registry, Some(waiter), "target", 12);
    let failed_target = ready_actor(&registry, Some(waiter), "failed target", 13);
    let cancelled_target = ready_actor(&registry, Some(waiter), "cancelled target", 14);

    let effects = tidepool_mcp::ensure_effects_module(&[tidepool_mcp::actor_decl()])
        .expect("materialize Actor effect module");
    let source = format!(
        r#"{{-# LANGUAGE DataKinds, FlexibleContexts, GADTs, OverloadedStrings, TypeOperators #-}}
module Expr where

import Prelude
import Tidepool.Actor
import Tidepool.Actor.Internal
import Tidepool.Effects
import Tidepool.Internal.ExitCell

refCell :: ExitCell String (Bool -> Bool)
refCell = newExitCell "pending"

ref :: ActorRef Maybe (Bool -> Bool)
ref = ActorRef {actor_id} {incarnation} refCell

failedRef :: ActorRef Maybe Int
failedRef = ActorRef {failed_actor_id} {failed_incarnation} (newExitCell ("failed pending" :: String))

cancelledRef :: ActorRef Maybe Int
cancelledRef = ActorRef {cancelled_actor_id} {cancelled_incarnation} (newExitCell ("cancelled pending" :: String))

result :: M Bool
result =
  case fillExitCell refCell not of
    () -> do
      first <- awaitExit ref
      second <- awaitExit ref
      failed <- awaitExit failedRef
      cancelled <- awaitExit cancelledRef
      pure $ case (first, second, failed, cancelled) of
        ( Completed f
          , Completed g
          , Failed (ActorFailure failureSummary)
          , Cancelled (CancelReason cancelSummary)
          ) -> f False && g False
            && failureSummary == "reviewer crashed"
            && cancelSummary == "owner shutdown"
        _ -> False
"#,
        actor_id = target.id.0,
        incarnation = target.incarnation.0,
        failed_actor_id = failed_target.id.0,
        failed_incarnation = failed_target.incarnation.0,
        cancelled_actor_id = cancelled_target.id.0,
        cancelled_incarnation = cancelled_target.incarnation.0,
    );
    let compiled = EvalHarness::new()
        .with_stdlib()
        .with_includes(effects.include_paths())
        .compile(&source, "result")
        .expect("compile typed actor wait");

    // The mock stack has no Actor handler. Mounting the actor selects
    // HandleOrSuspend, so the nominal wait request reaches the actor adapter
    // without assigning any meaning to its freer-simple row position.
    let mut session = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        AsSink(mock::min_stack()),
        TestSink,
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap actor session");

    let lease = mount_actor_turn(&registry, &mut session, waiter, ActorTurnKind::Haskell)
        .expect("mount first waiter turn");
    let first = session
        .run("typed_wait", &compiled.expr, &compiled.table)
        .expect("run to pending wait");
    let (first_hole, first_request) = match first {
        ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        ResidentOutcome::Completed { .. } => panic!("wait must suspend"),
    };
    drop(lease);

    let mut first_wait = ActorWait::register(&registry, waiter, &first_request, &compiled.table)
        .expect("register pending wait");
    assert!(first_wait.poll().expect("poll pending wait").is_none());

    let terminal = ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "job completed".into(),
    };
    registry
        .finish(target, terminal.clone())
        .expect("finish target");
    assert_eq!(
        first_wait.poll().expect("settle first wait"),
        Some(terminal.clone())
    );

    let lease = mount_actor_turn(&registry, &mut session, waiter, ActorTurnKind::Haskell)
        .expect("remount first continuation");
    let first_answer =
        actor_terminal_value(&terminal, &compiled.table).expect("encode completed status");
    let second = session
        .resume(first_hole, first_answer)
        .expect("resume first wait");
    let (second_hole, second_request) = match second {
        ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        ResidentOutcome::Completed { .. } => panic!("second wait must suspend independently"),
    };
    drop(lease);

    let mut late_wait = ActorWait::register(&registry, waiter, &second_request, &compiled.table)
        .expect("register late wait");
    assert_eq!(
        late_wait.poll().expect("late wait observes retained exit"),
        Some(terminal.clone())
    );

    let lease = mount_actor_turn(&registry, &mut session, waiter, ActorTurnKind::Haskell)
        .expect("remount second continuation");
    let second_answer =
        actor_terminal_value(&terminal, &compiled.table).expect("encode retained completed status");
    let third = session
        .resume(second_hole, second_answer)
        .expect("resume late wait");
    drop(lease);

    let (third_hole, third_request) = match third {
        ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        ResidentOutcome::Completed { .. } => panic!("failed target wait must suspend"),
    };
    let mut failed_wait = ActorWait::register(&registry, waiter, &third_request, &compiled.table)
        .expect("register failed-target wait");
    assert!(failed_wait
        .poll()
        .expect("poll failed-target wait")
        .is_none());
    let failed_terminal = ActorTerminal {
        kind: ActorExitKind::Failed,
        summary: "reviewer crashed".into(),
    };
    registry
        .finish(failed_target, failed_terminal.clone())
        .expect("fail target");
    assert_eq!(
        failed_wait.poll().expect("settle failed-target wait"),
        Some(failed_terminal.clone())
    );

    let lease = mount_actor_turn(&registry, &mut session, waiter, ActorTurnKind::Haskell)
        .expect("remount failed-target continuation");
    let failed_answer =
        actor_terminal_value(&failed_terminal, &compiled.table).expect("encode failed status");
    let fourth = session
        .resume(third_hole, failed_answer)
        .expect("resume failed-target wait");
    drop(lease);

    let (fourth_hole, fourth_request) = match fourth {
        ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        ResidentOutcome::Completed { .. } => panic!("cancelled target wait must suspend"),
    };
    let mut cancelled_wait =
        ActorWait::register(&registry, waiter, &fourth_request, &compiled.table)
            .expect("register cancelled-target wait");
    assert!(cancelled_wait
        .poll()
        .expect("poll cancelled-target wait")
        .is_none());
    let cancelled_terminal = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "owner shutdown".into(),
    };
    registry
        .finish(cancelled_target, cancelled_terminal.clone())
        .expect("cancel target");
    assert_eq!(
        cancelled_wait.poll().expect("settle cancelled-target wait"),
        Some(cancelled_terminal.clone())
    );

    let lease = mount_actor_turn(&registry, &mut session, waiter, ActorTurnKind::Haskell)
        .expect("remount cancelled-target continuation");
    let cancelled_answer = actor_terminal_value(&cancelled_terminal, &compiled.table)
        .expect("encode cancelled status");
    let completed = session
        .resume(fourth_hole, cancelled_answer)
        .expect("resume cancelled-target wait");
    drop(lease);

    match completed {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(true));
        }
        ResidentOutcome::Suspended { .. } => panic!("both waits should now be complete"),
    }
}
