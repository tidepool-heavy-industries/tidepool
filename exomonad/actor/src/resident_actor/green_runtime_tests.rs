//! Compiled Green bodies use the production native actor and Form interpreter.
//! The controlled host gates submissions after two actual FormAwait calls.
use super::*;
use crate::{FormHost, FormPublication};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tidepool_bridge_effects::{FormAttempt, FormAttemptId, FormCause, FormTransition};
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_testing::eval_harness;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Reverse,
    CancelSibling,
    CancelParent,
    Single,
}

#[derive(Default)]
struct HostState {
    mounts: BTreeMap<String, (ActorRef, String)>,
    awaited: BTreeSet<String>,
    committed: Vec<String>,
    closed: BTreeSet<String>,
}

struct Host {
    scenario: Scenario,
    state: Mutex<HostState>,
    both_waiting: tokio::sync::Semaphore,
    changed: tokio::sync::watch::Sender<()>,
}

impl Host {
    fn new(scenario: Scenario) -> Arc<Self> {
        Arc::new(Self {
            scenario,
            state: Mutex::new(Default::default()),
            both_waiting: tokio::sync::Semaphore::new(0),
            changed: tokio::sync::watch::channel(()).0,
        })
    }
    fn name(state: &HostState, actor: ActorRef, mount: &str) -> Result<String, FormCause> {
        match state.mounts.get(mount) {
            Some((owner, name)) if *owner == actor => Ok(name.clone()),
            _ => Err(FormCause::FormUnauthorized),
        }
    }
}

impl FormHost for Host {
    fn changed(&self) -> futures_util::future::BoxFuture<'static, Result<(), FormCause>> {
        let mut changed = self.changed.subscribe();
        Box::pin(async move {
            changed
                .changed()
                .await
                .map_err(|error| FormCause::FormTransportFailed(error.to_string()))
        })
    }
    fn open(
        &self,
        publication: &FormPublication,
        mount: &str,
        descriptor: &serde_json::Value,
    ) -> Result<(), FormCause> {
        let name = descriptor
            .as_str()
            .ok_or_else(|| FormCause::FormMalformed("controlled form name".into()))?;
        let mut state = self.state.lock();
        assert!(state
            .mounts
            .insert(mount.into(), (publication.actor, name.into()))
            .is_none());
        Ok(())
    }
    fn attempt(&self, actor: ActorRef, mount: &str) -> Result<Option<FormAttempt>, FormCause> {
        let mut state = self.state.lock();
        let name = Self::name(&state, actor, mount)?;
        if state.closed.contains(&name) {
            return Err(FormCause::FormClosed);
        }
        let inserted = state.awaited.insert(name.clone());
        if inserted && state.awaited.len() == 2 {
            self.both_waiting.add_permits(1);
            self.changed.send_replace(());
        }
        let ready = match self.scenario {
            Scenario::Single => true,
            Scenario::CancelParent => false,
            Scenario::Reverse => {
                state.awaited.len() == 2
                    && (name == "two" || state.committed.iter().any(|name| name == "two"))
            }
            Scenario::CancelSibling => state.awaited.len() == 2 && name == "two",
        };
        Ok(ready.then(|| {
            FormAttempt::FormSubmitted(
                FormAttemptId::FormAttemptToken(format!("{mount}:attempt")),
                serde_json::json!({"submitted": name}),
            )
        }))
    }
    fn reject(
        &self,
        _: ActorRef,
        _: &str,
        _: &str,
        _: &serde_json::Value,
    ) -> Result<FormTransition, FormCause> {
        panic!("these compiled fixtures have no validation rejection")
    }
    fn commit(
        &self,
        actor: ActorRef,
        mount: &str,
        attempt: &str,
        presentation: &serde_json::Value,
    ) -> Result<FormTransition, FormCause> {
        assert_eq!(attempt, format!("{mount}:attempt"));
        let mut state = self.state.lock();
        let name = Self::name(&state, actor, mount)?;
        assert_eq!(presentation.as_str(), Some(name.as_str()));
        if state.closed.contains(&name) {
            return Ok(FormTransition::FormStale);
        }
        assert!(
            !state.committed.contains(&name),
            "native continuation must commit once"
        );
        state.committed.push(name);
        self.changed.send_replace(());
        Ok(FormTransition::FormApplied)
    }
    fn close(&self, actor: ActorRef, mount: &str) -> Result<(), FormCause> {
        let mut state = self.state.lock();
        let name = Self::name(&state, actor, mount)?;
        assert!(
            !state.committed.contains(&name),
            "settled answer remains history, rather than closed pending input"
        );
        state.closed.insert(name);
        self.changed.send_replace(());
        Ok(())
    }
    fn display(&self, _: &FormPublication, _: u64, _: &serde_json::Value) -> Result<(), FormCause> {
        panic!("these fixtures do not emit rich views")
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    forest: ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>,
    host: Arc<Host>,
    effect_keys: Vec<crate::ActorEffectKey>,
}

impl Fixture {
    fn new(case: u64, scenario: Scenario) -> Self {
        eval_harness::require_extract();
        let mut declarations = vec![tidepool_mcp::console_decl(), tidepool_mcp::askuser_decl()];
        let mut effect_keys = vec![
            crate::ActorEffectKey::Console,
            crate::ActorEffectKey::AskUser,
        ];
        if scenario != Scenario::Single {
            declarations.push(tidepool_mcp::green_decl());
            effect_keys.push(crate::ActorEffectKey::Green);
        }
        let effects =
            tidepool_mcp::ensure_effects_module(&declarations).expect("declared effect module");
        let include = crate::resident_workbench::request_tests::fixture_include_roots(&effects);
        let preamble = tidepool_mcp::build_notebook_preamble(&declarations, false);
        let directory = tempfile::tempdir().expect("session source directory");
        let session = tidepool_repr::SessionId(u64::from(std::process::id()) * 10_000 + case);
        let lib = SessionLib::open(session, directory.path(), ModuleEnv::standalone_default())
            .expect("session declaration environment")
            .with_validation_include(include.clone());
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let (forest, _deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            None,
            crate::Incarnation::FIRST,
        );
        let host = Host::new(scenario);
        Self {
            _directory: directory,
            forest: forest.with_form_host(host.clone()),
            host,
            effect_keys,
        }
    }

    async fn parent(&self) -> LocalActorRef {
        let actor = self
            .forest
            .new_workbench(
                "green-native-fixture".into(),
                crate::ActorCapabilities::default().with_effect_keys(self.effect_keys.clone()),
            )
            .await
            .expect("native workbench actor");
        let setup = include_str!("green_runtime_setup.hs");
        let setup = if self.host.scenario == Scenario::Single {
            setup.to_string()
        } else {
            let (pragma, declarations) = setup.split_once('\n').expect("fixture language pragma");
            format!("{pragma}\nimport qualified Tidepool.Async as Async\n{declarations}")
        };
        let result = self.execute(&actor, &setup, None).await;
        let response = result.expect("checked fixture declarations");
        assert_eq!(
            response.status,
            WorkbenchRunStatus::Committed,
            "fixture declaration diagnostics: {response:?}"
        );
        actor
    }

    async fn execute(
        &self,
        actor: &LocalActorRef,
        source: &str,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> crate::KernelWorkbenchReply {
        let (reply, receive) = tokio::sync::oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Workbench {
                invocation: crate::ActorWorkbenchInvocation::unbound(
                    WorkbenchRequest::from_cell_input(source),
                ),
                control: control.clone(),
                reply: reply.into(),
            })
            .expect("actual native workbench admission");
        tokio::time::timeout(Duration::from_secs(240), async {
            match control {
                Some(control) => {
                    tokio::pin!(receive);
                    tokio::select! {
                        reply = &mut receive => reply.expect("actual resident settlement"),
                        waiting = self.host.both_waiting.acquire() => {
                            waiting.unwrap().forget();
                            assert!(control.request_cancellation(), "parent cancellation wins before either answer");
                            receive.await.expect("cancelled native settlement")
                        }
                    }
                }
                None => receive.await.expect("actual resident settlement"),
            }
        }).await.expect("bounded compiled native fixture")
    }

    fn assert_live_leases_released(&self) {
        assert!(
            self.forest
                .environment
                .form_registry
                .lock()
                .values()
                .all(|form| form.upgrade().is_none()),
            "native frame retirement releases decoder ownership, including siblings"
        );
    }

    async fn finish(self) {
        assert!(self
            .forest
            .shutdown()
            .await
            .iter()
            .all(crate::ForestRootShutdown::is_confirmed));
    }
}

fn assert_true(reply: crate::KernelWorkbenchReply) {
    let response = reply.expect("compiled Green fixture");
    assert_eq!(
        response.status,
        WorkbenchRunStatus::Committed,
        "{response:?}"
    );
    assert_eq!(
        response.items.last().map(|item| item.output.trim()),
        Some("True"),
        "{response:?}"
    );
}

#[tokio::test]
async fn compiled_concurrent_forms_reverse_settlement_preserves_original_heap_closures() {
    let fixture = Fixture::new(745, Scenario::Reverse);
    let actor = fixture.parent().await;
    assert_true(
        fixture
            .execute(&actor, include_str!("green_runtime_reverse.hs"), None)
            .await,
    );
    assert_eq!(fixture.host.state.lock().committed, ["two", "one"]);
    assert!(fixture.host.state.lock().closed.is_empty());
    fixture.assert_live_leases_released();
    fixture.finish().await;
}

#[tokio::test]
async fn compiled_green_cancel_closes_only_sibling_and_preserves_selected_function() {
    let fixture = Fixture::new(746, Scenario::CancelSibling);
    let actor = fixture.parent().await;
    assert_true(
        fixture
            .execute(&actor, include_str!("green_runtime_cancel.hs"), None)
            .await,
    );
    assert_eq!(fixture.host.state.lock().committed, ["two"]);
    assert_eq!(
        fixture.host.state.lock().closed,
        BTreeSet::from(["one".to_string()])
    );
    fixture.assert_live_leases_released();
    fixture.finish().await;
}

#[tokio::test]
async fn compiled_green_parent_cancellation_releases_both_pending_native_forms() {
    let fixture = Fixture::new(747, Scenario::CancelParent);
    let actor = fixture.parent().await;
    let control = crate::WorkbenchExecutionControl::untracked();
    let result = fixture
        .execute(
            &actor,
            include_str!("green_runtime_parent_cancel.hs"),
            Some(control.clone()),
        )
        .await;
    assert!(
        result
            .as_ref()
            .map_or(true, |reply| reply.status != WorkbenchRunStatus::Committed),
        "{result:?}"
    );
    assert!(matches!(
        control.cancellation_outcome(control.execution_id(actor.identity()), result),
        crate::WorkbenchCancellationOutcome::Cancelled { .. }
    ));
    assert!(fixture.host.state.lock().committed.is_empty());
    assert_eq!(
        fixture.host.state.lock().closed,
        BTreeSet::from(["one".to_string(), "two".to_string()])
    );
    fixture.assert_live_leases_released();
    fixture.finish().await;
}

#[tokio::test]
async fn compiled_single_form_uses_original_non_green_frontier() {
    let fixture = Fixture::new(748, Scenario::Single);
    let actor = fixture.parent().await;
    assert_true(
        fixture
            .execute(&actor, include_str!("green_runtime_single.hs"), None)
            .await,
    );
    assert_eq!(fixture.host.state.lock().committed, ["single"]);
    fixture.assert_live_leases_released();
    fixture.finish().await;
}

#[tokio::test]
async fn compiled_green_evaluation_failure_is_not_success_or_authored_cancellation() {
    let fixture = Fixture::new(749, Scenario::Single);
    let actor = fixture.parent().await;
    let control = crate::WorkbenchExecutionControl::untracked();
    let result = fixture
        .execute(
            &actor,
            include_str!("green_runtime_failure.hs"),
            Some(control.clone()),
        )
        .await;
    assert!(
        result
            .as_ref()
            .map_or(true, |reply| reply.status != WorkbenchRunStatus::Committed),
        "{result:?}"
    );
    assert!(!control.cancellation_requested());
    fixture.assert_live_leases_released();
    fixture.finish().await;
}
