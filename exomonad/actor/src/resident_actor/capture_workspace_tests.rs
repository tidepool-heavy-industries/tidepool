//! Exact checkpoint custody across idle-spec spawn and the workspace await.

use super::*;
use crate::fork_workspace::{
    PreparedWorkspaceAttachment, WorkspaceAdmission, WorkspaceAdmissionError,
    WorkspaceAdmissionFuture, WorkspaceCustody, WorkspaceSelection,
};
use exomonad_tool::{ToolArguments, ToolInvocation};
use futures_util::{stream::FuturesUnordered, StreamExt};
use tidepool_bridge_effects::{
    WtBranchName, WtGitOid, WtWorktreeHandle, WtWorktreeId, WtWorktreeReceipt,
};
use tidepool_runtime::session::{
    insert_preamble_imports, ContextCheckpointBoundary, ModuleEnv, SessionLib, WorkbenchRequest,
};
use tidepool_testing::eval_harness;

struct PausedWorkspace {
    entered: mpsc::UnboundedSender<String>,
    release: tokio::sync::Semaphore,
    reject: std::sync::atomic::AtomicBool,
}

struct TestWorkspaceCustody;
impl WorkspaceCustody for TestWorkspaceCustody {
    fn actor_stopped(&self, _: &ActorTerminal) {}
    fn process_may_exist(&self) {}
}

impl WorkspaceAdmission for PausedWorkspace {
    fn prepare(
        &self,
        _: ActorRef,
        selection: WorkspaceSelection,
        _: Option<crate::WorkspaceAccess>,
    ) -> WorkspaceAdmissionFuture<'_> {
        assert!(matches!(selection, WorkspaceSelection::ForkDirectory(_)));
        Box::pin(async move {
            let workspace = format!("capture-workspace-{}", uuid::Uuid::new_v4());
            self.entered
                .send(workspace.clone())
                .expect("workspace observer");
            self.release
                .acquire()
                .await
                .expect("workspace release")
                .forget();
            if self.reject.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(WorkspaceAdmissionError {
                    detail: "controlled parent continuation failure".into(),
                });
            }
            Ok(PreparedWorkspaceAttachment::new(
                WtWorktreeHandle {
                    handle_receipt: WtWorktreeReceipt {
                        tree_id: WtWorktreeId { raw: workspace },
                        cwd: "/fixture-checkpoint-workspace".into(),
                        branch: Some(WtBranchName {
                            raw: "checkpoint-readers".into(),
                        }),
                        source_head: WtGitOid {
                            raw: "0123456789012345678901234567890123456789".into(),
                        },
                        snapshot_ref: None,
                        created_at: 0,
                    },
                },
                |_| Ok(Arc::new(TestWorkspaceCustody)),
            ))
        })
    }
}

async fn run_cell(actor: LocalActorRef, text: String) -> serde_json::Value {
    run_cell_with_context(actor, text, None).await
}

async fn run_cell_with_context(
    actor: LocalActorRef,
    text: String,
    context: Option<exomonad_tool::ToolInvocationContext>,
) -> serde_json::Value {
    tokio::time::timeout(
        std::time::Duration::from_secs(240),
        crate::resident_tools::ResidentToolClient::local(actor)
            .dispatch_workbench(WorkbenchRequest::from_cell_input(&text), context),
    )
    .await
    .expect("production notebook admission bounded")
    .expect("production notebook admission")
}

fn assert_committed(reply: &serde_json::Value) {
    assert_eq!(reply["status"], "committed", "{reply:?}");
}

fn reader_source(token: &str, label: &str, lifetime: &str) -> String {
    include_str!("capture_workspace_child.hs")
        .replace("CHECKPOINT_TOKEN", token)
        .replace("CHILD_LABEL", label)
        .replace("CHILD_LIFETIME", lifetime)
}

fn reader_steps(token: &str, children: &[(&str, &str)]) -> String {
    let mut source = String::from("do\n");
    for (label, lifetime) in children {
        source.push_str("  _ <- do\n");
        for line in reader_source(token, label, lifetime).lines().skip(1) {
            source.push_str("  ");
            source.push_str(line);
            source.push('\n');
        }
    }
    source.push_str("  pure True\n");
    source
}

struct CaptureFixture {
    _root: tempfile::TempDir,
    forest: ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>,
    deployments: mpsc::Receiver<LocalResidentDeployment>,
    workspaces: Arc<PausedWorkspace>,
    entered: mpsc::UnboundedReceiver<String>,
    session: tidepool_repr::SessionId,
}

impl CaptureFixture {
    fn new(case: u64, permits: usize) -> Self {
        eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::agent_tools_decl(),
            tidepool_mcp::agent_launch_decl(),
            tidepool_mcp::actor_decl(),
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
            tidepool_mcp::fs_read_decl(),
            tidepool_mcp::worktree_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("effect module");
        let mut include = effects.include_paths().to_vec();
        include.push(eval_harness::prelude_path());
        let preamble = insert_preamble_imports(
            &tidepool_mcp::build_notebook_preamble(&declarations, false),
            "Tidepool.Agent.Contract",
        );
        let preamble =
            insert_preamble_imports(&preamble, "qualified Tidepool.Effects.Core as Core");
        let root = tempfile::tempdir().expect("session root");
        let session = tidepool_repr::SessionId(u64::from(std::process::id()) * 10_000 + case);
        let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .expect("declaration plane")
            .with_validation_include(include.clone());
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let (entered, entered_rx) = mpsc::unbounded_channel();
        let workspaces = Arc::new(PausedWorkspace {
            entered,
            release: tokio::sync::Semaphore::new(permits),
            reject: std::sync::atomic::AtomicBool::new(false),
        });
        let (forest, deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            Some(workspaces.clone()),
            crate::Incarnation::FIRST,
        );
        Self {
            _root: root,
            forest,
            deployments,
            workspaces,
            entered: entered_rx,
            session,
        }
    }

    async fn parent(&self, label: &str) -> LocalActorRef {
        let parent = self
            .forest
            .new_workbench(
                label.into(),
                crate::ActorCapabilities::default()
                    .with_effect_keys(vec![crate::ActorEffectKey::AgentLaunch]),
            )
            .await
            .expect("capture fixture parent");
        assert_committed(
            &run_cell(
                parent.clone(),
                include_str!("capture_workspace_declarations.hs").into(),
            )
            .await,
        );
        parent
    }

    async fn checkpoint(
        &self,
        issuer: &LocalActorRef,
        source: crate::CheckpointSourceLayer,
    ) -> (String, tidepool_codegen::scope::ScopeId) {
        assert_committed(&run_cell(issuer.clone(), "let capturedValue = 41 :: Int".into()).await);
        let context = self
            .forest
            .directory
            .session_context(issuer.identity())
            .expect("issuer context");
        let (scope, retained) = self
            .forest
            .environment
            .runner
            .capture_retained_context_scope(context)
            .await
            .expect("real native checkpoint lease");
        let token = self
            .forest
            .environment
            .actor_admissions
            .capture_checkpoint_with_retained_scope(
                "completed-private".into(),
                issuer.identity(),
                crate::ActorCapabilities::default()
                    .with_effect_keys(vec![crate::ActorEffectKey::AgentLaunch]),
                None,
                None,
                source,
                self.session,
                scope,
                ContextCheckpointBoundary::external(
                    "native-capture".into(),
                    "capture-request".into(),
                    "completed-capture".into(),
                ),
                None,
                retained,
                crate::ActorPersistencePolicy::Ephemeral,
            );
        self.forest
            .environment
            .actor_admissions
            .settle_checkpoint(&token, self.session, true)
            .expect("checkpoint publication");
        (token, scope)
    }

    async fn release_checkpoint(&self, token: &str, scope: tidepool_codegen::scope::ScopeId) {
        let retired = self
            .forest
            .environment
            .actor_admissions
            .release_checkpoint(token, self.session)
            .expect("token release")
            .expect("original checkpoint root");
        assert_eq!(retired, scope);
        self.forest
            .environment
            .runner
            .retire_checkpoint_scopes(self.session, vec![scope])
            .await
            .expect("original root retirement");
        self.forest
            .environment
            .actor_admissions
            .confirm_checkpoint_release(token, self.session, scope)
            .expect("checkpoint release confirmation");
    }

    async fn workspace_wait(&mut self) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(240), self.entered.recv())
            .await
            .expect("workspace admission reaches its explicit wait")
            .expect("workspace observer remains live")
    }

    async fn child(&mut self, label: &str) -> Box<LocalResidentInstallation> {
        self.child_matching(&[label]).await
    }

    async fn child_matching(&mut self, labels: &[&str]) -> Box<LocalResidentInstallation> {
        tokio::time::timeout(std::time::Duration::from_secs(240), async {
            loop {
                match self.deployments.recv().await.expect("deployment observer") {
                    LocalResidentDeployment::PolicyInstalled(child) => {
                        assert!(
                            labels.contains(&child.label.as_str()),
                            "unexpected child policy: {}",
                            child.label
                        );
                        child
                            .spawn_admission
                            .as_ref()
                            .expect("exact idle spawn admission")
                            .acknowledge(child.actor.identity())
                            .expect("attachment acknowledgement");
                        return child;
                    }
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        assert!(
                            !self
                                .forest
                                .environment
                                .actors
                                .lock()
                                .get(&actor)
                                .is_some_and(|record| labels.contains(&record.descriptor.label())),
                            "child retired before installation: {terminal:?}"
                        );
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("child readiness")
    }

    async fn finish(self) {
        assert!(self
            .forest
            .shutdown()
            .await
            .iter()
            .all(crate::ForestRootShutdown::is_confirmed));
        assert_eq!(
            self.forest
                .measurement_snapshot()
                .expect("shared native session")
                .parked,
            Some(0)
        );
    }
}

async fn assert_captured_reader(child: &LocalResidentInstallation) {
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(240),
        child.policy.dispatch_boxed(ToolInvocation {
            context: None,
            name: "ping".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        }),
    )
    .await
    .expect("handler request bounded")
    .expect("captured handler request");
    let crate::ResidentToolResponse::Workbench(reply) = reply else {
        panic!("installed handler retains a typed workbench receipt: {reply:?}");
    };
    assert_eq!(reply.status, WorkbenchRunStatus::Committed, "{reply:?}");
    let [item] = reply.items.as_slice() else {
        panic!("one captured handler result: {reply:?}");
    };
    assert_eq!(item.status, WorkbenchItemStatus::Committed, "{reply:?}");
    assert_eq!(item.output.trim(), "52", "{reply:?}");
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(240),
        child.policy.dispatch_boxed(ToolInvocation {
            context: None,
            name: crate::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(
                concat!(
                    "if capturedValue + 1 == (42 :: Int) ",
                    "then (pure () :: Eff '[] ()) ",
                    "else Tidepool.Effects.error \"reader lost the original checkpoint value 41\""
                )
                .into(),
            ),
        }),
    )
    .await
    .expect("checkpoint notebook request bounded")
    .expect("checkpoint notebook executes the original-value assertion")
    .into_json()
    .expect("notebook receipt");
    assert_committed(&reply);
    let [item] = reply["items"].as_array().expect("item receipts").as_slice() else {
        panic!("one original-value assertion receipt: {reply:?}");
    };
    assert_eq!(item["status"], "committed", "{reply:?}");
}

async fn stop_reader(child: Box<LocalResidentInstallation>) {
    let stopped = child
        .actor
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "captured reader done".into(),
            diagnostic: None,
        })
        .await
        .expect("reader retirement");
    assert!(stopped.cleanup.is_confirmed(), "{:?}", stopped.cleanup);
}

#[tokio::test]
async fn two_checkpoint_children_remint_after_workspace_wait_token_release_and_issuer_failure() {
    let mut fixture = CaptureFixture::new(184, 0);
    let issuer = fixture.parent("checkpoint-issuer").await;
    let (token, scope) = fixture
        .checkpoint(&issuer, crate::CheckpointSourceLayer::default())
        .await;
    let launcher = fixture.parent("shared-launcher").await;
    // This binding belongs to the launcher and must not replace the issuer snapshot.
    assert_committed(&run_cell(launcher.clone(), "let capturedValue = 900 :: Int".into()).await);
    let mut calls = FuturesUnordered::new();
    for label in ["first", "second"] {
        calls.push(tokio::spawn(run_cell_with_context(
            launcher.clone(),
            reader_source(&token, label, "Core.ActorOwned"),
            Some(exomonad_tool::ToolInvocationContext::external(
                "capture-fixture".into(),
                label.into(),
                "haskell".into(),
                None,
                None,
            )),
        )));
    }
    let first_workspace = fixture.workspace_wait().await;
    let second_workspace = fixture.workspace_wait().await;
    assert_ne!(first_workspace, second_workspace);
    assert!(calls.iter().all(|call| !call.is_finished()));
    assert_committed(&run_cell(launcher.clone(), "pure (42 :: Int)".into()).await);
    assert!(calls.iter().all(|call| !call.is_finished()));
    fixture.release_checkpoint(&token, scope).await;
    let stopped = issuer
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "issuer failed after admission".into(),
            diagnostic: None,
        })
        .await
        .expect("issuer failure");
    assert!(stopped.cleanup.is_confirmed(), "{:?}", stopped.cleanup);
    assert_eq!(issuer.terminal().wait().await.kind, ActorExitKind::Failed);
    fixture.workspaces.release.add_permits(2);
    let mut children = Vec::new();
    let mut completed_calls = 0;
    // Readiness can arrive in either order. A failed launch caller or matching
    // child retirement must report its cause instead of consuming the wait bound.
    tokio::time::timeout(std::time::Duration::from_secs(240), async {
        while children.len() < 2 {
            tokio::select! {
                biased;
                child = fixture.child_matching(&["first", "second"]) => {
                    children.push(child);
                }
                outcome = calls.next(), if !calls.is_empty() => {
                    let reply = outcome
                        .expect("pending launch caller")
                        .expect("launch caller failed before child readiness");
                    assert_committed(&reply);
                    completed_calls += 1;
                }
            }
        }
    })
    .await
    .expect("both independently admitted children become ready");
    while let Some(outcome) = calls.next().await {
        assert_committed(&outcome.expect("launch caller"));
        completed_calls += 1;
    }
    assert_eq!(
        completed_calls, 2,
        "both launch callers settle successfully"
    );
    assert_ne!(children[0].actor.identity(), children[1].actor.identity());
    for child in &children {
        assert_captured_reader(child).await;
    }
    for child in children {
        stop_reader(child).await;
    }
    fixture.finish().await;
}

#[tokio::test]
async fn two_captured_readers_reply_before_parent_failure_and_survive_final_checkpoint_release() {
    let mut fixture = CaptureFixture::new(185, 2);
    let parent = fixture.parent("private-capture-parent").await;
    let (token, scope) = fixture
        .checkpoint(&parent, crate::CheckpointSourceLayer::default())
        .await;
    let source = reader_steps(
        &token,
        &[
            ("first", "Core.ActorOwned"),
            ("second", "Core.ActorOwned"),
            ("failure-barrier", "Core.ActorOwned"),
        ],
    );
    let client = crate::resident_tools::ResidentToolClient::local(parent.clone());
    let call = tokio::spawn(async move {
        client
            .dispatch_workbench(WorkbenchRequest::from_cell_input(&source), None)
            .await
    });
    let first = fixture.child("first").await;
    let second = fixture.child("second").await;
    assert_ne!(first.actor.identity(), second.actor.identity());
    for child in [&first, &second] {
        assert_captured_reader(child).await;
        assert!(
            !call.is_finished(),
            "reader replies precede the parent cell failure"
        );
    }
    for _ in 0..3 {
        fixture.workspace_wait().await;
    }
    assert!(!call.is_finished());
    fixture
        .workspaces
        .reject
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fixture.workspaces.release.add_permits(1);
    let failure = tokio::time::timeout(std::time::Duration::from_secs(30), call)
        .await
        .expect("workspace refusal settles caller")
        .expect("caller task")
        .expect_err("controlled workspace refusal fails the cell");
    assert!(
        format!("{failure:?}").contains("controlled parent continuation failure"),
        "{failure:?}"
    );
    assert!(parent.terminal().get().is_none());
    fixture
        .forest
        .environment
        .actor_admissions
        .checkpoint(&token, fixture.session)
        .expect("completed capture survives cell failure");
    for child in [&first, &second] {
        assert_captured_reader(child).await;
    }
    fixture
        .workspaces
        .reject
        .store(false, std::sync::atomic::Ordering::SeqCst);
    fixture.workspaces.release.add_permits(1);
    let reuse = tokio::spawn(run_cell(
        parent.clone(),
        reader_source(&token, "reused", "Core.ActorOwned"),
    ));
    let reused = fixture.child("reused").await;
    assert_committed(&reuse.await.unwrap());
    fixture.release_checkpoint(&token, scope).await;
    for child in [&first, &second, &reused] {
        assert_captured_reader(child).await;
    }
    stop_reader(first).await;
    assert_captured_reader(&second).await;
    stop_reader(second).await;
    assert_captured_reader(&reused).await;
    stop_reader(reused).await;
    fixture.finish().await;
}

#[tokio::test]
async fn partial_idle_spawn_failure_cleans_invocation_child_and_preserves_capture() {
    let mut fixture = CaptureFixture::new(186, 1);
    let parent = fixture.parent("partial-capture-parent").await;
    let (token, scope) = fixture
        .checkpoint(&parent, crate::CheckpointSourceLayer::default())
        .await;
    let launch = tokio::spawn(run_cell(
        parent.clone(),
        reader_source(&token, "survivor", "Core.ActorOwned"),
    ));
    let survivor = fixture.child("survivor").await;
    assert_committed(&launch.await.unwrap());
    fixture.workspace_wait().await;
    assert_captured_reader(&survivor).await;
    fixture.workspaces.release.add_permits(1);
    let source = reader_steps(
        &token,
        &[
            ("partial-first", "Core.InvocationOwned"),
            ("partial-second", "Core.InvocationOwned"),
        ],
    );
    let client = crate::resident_tools::ResidentToolClient::local(parent.clone());
    let call = tokio::spawn(async move {
        client
            .dispatch_workbench(WorkbenchRequest::from_cell_input(&source), None)
            .await
    });
    let first = fixture.child("partial-first").await;
    fixture.workspace_wait().await;
    fixture.workspace_wait().await;
    assert!(!call.is_finished());
    assert!(first.actor.terminal().get().is_none());
    assert_captured_reader(&first).await;
    fixture
        .workspaces
        .reject
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fixture.workspaces.release.add_permits(1);
    let failure = tokio::time::timeout(std::time::Duration::from_secs(30), call)
        .await
        .expect("second independent refusal settles caller")
        .unwrap()
        .expect_err("second independent start refuses");
    assert!(
        format!("{failure:?}").contains("controlled parent continuation failure"),
        "{failure:?}"
    );
    assert!(parent.terminal().get().is_none());
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            first.actor.terminal().wait(),
        )
        .await
        .expect("invocation child actually retires")
        .kind,
        ActorExitKind::Cancelled
    );
    let cleanup = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(cleanup) = first.actor.terminal().cleanup() {
                break cleanup;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("invocation child cleanup retained");
    assert!(cleanup.is_confirmed(), "{cleanup:?}");
    fixture
        .forest
        .environment
        .actor_admissions
        .checkpoint(&token, fixture.session)
        .expect("completed capture remains admitted");
    assert_captured_reader(&survivor).await;
    fixture
        .workspaces
        .reject
        .store(false, std::sync::atomic::Ordering::SeqCst);
    fixture.workspaces.release.add_permits(1);
    let reuse = tokio::spawn(run_cell(
        parent.clone(),
        reader_source(&token, "fresh-reader", "Core.ActorOwned"),
    ));
    let fresh = fixture.child("fresh-reader").await;
    assert_committed(&reuse.await.unwrap());
    assert_ne!(fresh.actor.identity(), first.actor.identity());
    fixture.release_checkpoint(&token, scope).await;
    assert_captured_reader(&survivor).await;
    assert_captured_reader(&fresh).await;
    stop_reader(survivor).await;
    assert_captured_reader(&fresh).await;
    stop_reader(fresh).await;
    fixture.finish().await;
}

struct EmptyRetainedSource;
impl crate::RetainedSourceLayer for EmptyRetainedSource {
    fn identities(&self) -> &[String] {
        &[]
    }
    fn include_paths(&self) -> &[std::path::PathBuf] {
        &[]
    }
}

struct EmptySourceAuthority;
impl crate::ActorSourceLayers for EmptySourceAuthority {
    fn stage_spec_reload(
        self: Arc<Self>,
        _: tidepool_repr::PrincipalId,
        _: &[String],
    ) -> Result<Box<dyn crate::StagedActorSourceReload>, crate::SourceLayerReload> {
        Err(crate::SourceLayerReload::Unavailable(
            "fixture owns no reload candidate".into(),
        ))
    }
}

#[tokio::test]
async fn checkpoint_issuer_source_authority_is_checked_before_workspace_effects() {
    let mut fixture = CaptureFixture::new(187, 1);
    fixture
        .forest
        .set_source_layers(Arc::new(EmptySourceAuthority));
    let parent = fixture.parent("source-authority-parent").await;
    // The launcher owns the accepted empty source. Only the checkpoint source
    // belongs to a different configured issuer, so checking the launcher alone
    // cannot establish authority for this child.
    let foreign = crate::SourceLayerIssuer::default().issue(Arc::new(EmptyRetainedSource));
    let (token, scope) = fixture.checkpoint(&parent, foreign).await;
    let failure = tokio::time::timeout(
        std::time::Duration::from_secs(240),
        crate::resident_tools::ResidentToolClient::local(parent).dispatch_workbench(
            WorkbenchRequest::from_cell_input(&reader_source(
                &token,
                "foreign-source-reader",
                "Core.ActorOwned",
            )),
            None,
        ),
    )
    .await
    .expect("checkpoint source refusal settles without child readiness")
    .expect_err("foreign checkpoint source refuses this launch");
    assert!(
        format!("{failure:?}").contains("host did not issue this source authority"),
        "{failure:?}"
    );
    assert!(
        matches!(
            fixture.entered.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "workspace preparation must not run before checkpoint source authentication"
    );
    assert_eq!(fixture.workspaces.release.available_permits(), 1);
    fixture.release_checkpoint(&token, scope).await;
    fixture.finish().await;
}
