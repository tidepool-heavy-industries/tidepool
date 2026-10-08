//! Production Form and Jev boundaries retain original typed action closures.
//! The response was encoded by upstream Proto.stub from the prepared request;
//! see jev_form_runtime_request.json and upstream Jev revision 2883fdc38cc7.
use super::*;
use crate::{FormHost, FormPublication, JevBackend, JevCallFailure};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tidepool_bridge_effects::{FormAttempt, FormAttemptId, FormCause, FormTransition};
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_testing::eval_harness;

struct MountedForm {
    actor: ActorRef,
    descriptor: serde_json::Value,
    committed: bool,
}

#[derive(Default)]
struct HostState {
    forms: BTreeMap<String, MountedForm>,
    committed: Vec<String>,
    views: Vec<serde_json::Value>,
}

#[derive(Default)]
struct Host(Mutex<HostState>);

impl FormHost for Host {
    fn changed(&self) -> futures_util::future::BoxFuture<'static, Result<(), FormCause>> {
        Box::pin(std::future::pending())
    }

    fn open(
        &self,
        publication: &FormPublication,
        mount: &str,
        descriptor: &serde_json::Value,
    ) -> Result<(), FormCause> {
        assert_eq!(descriptor["version"], 1);
        assert_eq!(descriptor["root"]["kind"], "choice");
        assert!(self
            .0
            .lock()
            .forms
            .insert(
                mount.into(),
                MountedForm {
                    actor: publication.actor,
                    descriptor: descriptor.clone(),
                    committed: false,
                }
            )
            .is_none());
        Ok(())
    }

    fn attempt(&self, actor: ActorRef, mount: &str) -> Result<Option<FormAttempt>, FormCause> {
        let state = self.0.lock();
        let form = state.forms.get(mount).expect("actual mounted form");
        assert_eq!(form.actor, actor);
        assert!(!form.committed, "settled forms must not be awaited again");
        // Draft identities come from the production descriptor, rather than
        // duplicating Form's occurrence-ID encoding in the scripted host.
        let root = &form.descriptor["root"];
        let field = root["id"].as_str().expect("choice occurrence ID");
        let first = root["options"][0]["id"].clone();
        let draft = serde_json::Value::Object(serde_json::Map::from_iter([(field.into(), first)]));
        Ok(Some(FormAttempt::FormSubmitted(
            FormAttemptId::FormAttemptToken(format!("{mount}:attempt")),
            draft,
        )))
    }

    fn reject(
        &self,
        _: ActorRef,
        _: &str,
        _: &str,
        _: &serde_json::Value,
    ) -> Result<FormTransition, FormCause> {
        panic!("descriptor-derived first option must pass the real Form decoder")
    }

    fn commit(
        &self,
        actor: ActorRef,
        mount: &str,
        attempt: &str,
        presentation: &serde_json::Value,
    ) -> Result<FormTransition, FormCause> {
        assert_eq!(attempt, format!("{mount}:attempt"));
        assert_eq!(presentation["kind"], "caption");
        let mut state = self.0.lock();
        let form = state.forms.get_mut(mount).expect("actual mounted form");
        assert_eq!(form.actor, actor);
        assert!(!form.committed, "each native form commits once");
        form.committed = true;
        let label = form.descriptor["root"]["label"]
            .as_str()
            .unwrap()
            .to_owned();
        state.committed.push(label);
        Ok(FormTransition::FormApplied)
    }

    fn close(&self, _: ActorRef, _: &str) -> Result<(), FormCause> {
        panic!("successful form answers must stay in history")
    }

    fn display(
        &self,
        _: &FormPublication,
        _: u64,
        view: &serde_json::Value,
    ) -> Result<(), FormCause> {
        self.0.lock().views.push(view.clone());
        Ok(())
    }
}

struct Jev {
    requests: Mutex<Vec<serde_json::Value>>,
    failure: bool,
}

impl JevBackend for Jev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        let request: serde_json::Value =
            serde_json::from_str(&request).expect("production Jev request encoder emits JSON");
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("jev_form_runtime_request.json")).unwrap();
        assert_eq!(
            request, expected,
            "prepared request must preserve the exact wire contract"
        );
        self.requests.lock().push(request);
        let response = if self.failure {
            Err(JevCallFailure::JevCircuitOpen(503, 987))
        } else {
            Ok(include_str!("jev_form_runtime_response.json").to_owned())
        };
        Box::pin(async move { response })
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    forest: ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>,
    host: Arc<Host>,
    jev: Arc<Jev>,
}

impl Fixture {
    fn new(case: u64, failure: bool) -> Self {
        eval_harness::require_extract();
        let declarations = vec![
            tidepool_mcp::console_decl(),
            tidepool_mcp::askuser_decl(),
            tidepool_mcp::jev_decl(),
        ];
        let effects =
            tidepool_mcp::ensure_effects_module(&declarations).expect("declared effect module");
        let mut include = crate::resident_workbench::request_tests::fixture_include_roots(&effects);
        let jev_sources = std::env::var_os("TIDEPOOL_JEV_SOURCE_DIR")
            .expect("owning native test action must declare pinned Jev sources");
        let core = PathBuf::from(jev_sources)
            .join("core")
            .canonicalize()
            .expect("pinned Core source root");
        assert!(core.join("Jev/Core.hs").is_file());
        include.push(core);
        let workspace = PathBuf::from(std::env::var_os("TIDEPOOL_JEV_WORKSPACE_DIR")
            .expect("owning native action must declare the public Jev workspace sources"))
            .canonicalize().expect("declared Jev workspace root");
        assert!(workspace.join("Jev/Operators.hs").is_file());
        include.push(workspace);
        let preamble = format!(
            "{}\nimport qualified Jev.Operators as J\nimport qualified Tidepool.Form as F\nimport qualified Tidepool.View as V\nimport Data.List.NonEmpty (NonEmpty(..))\nimport Control.Monad (foldM, forM_)\nimport Tidepool.Inspection.Display (Display(..))\n",
            tidepool_mcp::build_notebook_preamble(&declarations, false),
        );
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
        let (mut forest, _) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            None,
            crate::Incarnation::FIRST,
        );
        let host = Arc::new(Host::default());
        let jev = Arc::new(Jev {
            requests: Mutex::new(Vec::new()),
            failure,
        });
        forest.set_jev_backend(jev.clone());
        Self {
            _directory: directory,
            forest: forest.with_form_host(host.clone()),
            host,
            jev,
        }
    }

    async fn execute(&self, source: &str) {
        let actor = self
            .forest
            .new_workbench(
                "jev-form-native-fixture".into(),
                crate::ActorCapabilities::default().with_effect_keys(vec![
                    crate::ActorEffectKey::Console,
                    crate::ActorEffectKey::AskUser,
                    crate::ActorEffectKey::Jev,
                ]),
            )
            .await
            .expect("native actor admission");
        let (reply, receive) = tokio::sync::oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Workbench {
                invocation: crate::ActorWorkbenchInvocation::unbound(
                    WorkbenchRequest::from_cell_input(source),
                ),
                control: None,
                reply: reply.into(),
            })
            .expect("actual native workbench admission");
        let response = tokio::time::timeout(Duration::from_secs(300), receive)
            .await
            .expect("bounded native Jev/Form fixture")
            .expect("resident settlement")
            .expect("compiled native fixture");
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
        assert!(
            self.forest
                .environment
                .form_registry
                .lock()
                .values()
                .all(|form| form.upgrade().is_none()),
            "settled native frame releases all retained form decoders"
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

#[tokio::test]
async fn compiled_form_choice_preserves_prepared_jev_decoder_and_selected_action() {
    let fixture = Fixture::new(752, false);
    fixture
        .execute(include_str!("jev_form_runtime_success.hs"))
        .await;
    assert_eq!(fixture.jev.requests.lock().len(), 1);
    {
        let state = fixture.host.0.lock();
        assert_eq!(state.committed, ["Prepared plan", "Selected follow-up"]);
        assert_eq!(
            state.views.len(),
            1,
            "only the selected action emits a view"
        );
        let view = state.views[0].to_string();
        assert!(view.contains("original-quick"), "{view}");
        assert!(!view.contains("unselected"), "{view}");
    }
    fixture.finish().await;
}

#[tokio::test]
async fn compiled_form_jev_transport_refusal_preserves_cause_and_runs_no_action() {
    let fixture = Fixture::new(753, true);
    fixture
        .execute(include_str!("jev_form_runtime_failure.hs"))
        .await;
    assert_eq!(fixture.jev.requests.lock().len(), 1);
    {
        let state = fixture.host.0.lock();
        assert_eq!(state.committed, ["Prepared plan"]);
        assert_eq!(state.views.len(), 1);
        let view = state.views[0].to_string();
        assert!(view.contains("typed-circuit-open"), "{view}");
        assert!(!view.contains("unselected"), "{view}");
    }
    fixture.finish().await;
}

#[tokio::test]
async fn compiled_public_jev_response_histories_preserve_evidence_and_explicit_effect_counts() {
    let fixture = Fixture::new(754, false);
    fixture.execute(include_str!("jev_response_semantics.hs")).await;
    // Forty-nine two-operation histories, plus the deliberate Execute/Run/Run
    // sequence: only explicit Execute operations issue provider requests.
    assert_eq!(fixture.jev.requests.lock().len(), 15);
    {
        let state = fixture.host.0.lock();
        assert!(state.forms.is_empty(), "inspection and actions do not ask for forms");
        assert_eq!(state.views.len(), 16, "only explicit Run operations execute actions");
        assert!(state.views.iter().all(|view| view.to_string().contains("selected-action")));
    }
    fixture.finish().await;
}
