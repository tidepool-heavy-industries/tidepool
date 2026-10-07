//! Exact checkpoint scope admission across the production workspace await.

use super::*;
use crate::fork_workspace::{
    ForkWorkspaceAdmission, ForkWorkspaceAdmissionError, ForkWorkspaceAdmissionFuture,
    ForkWorkspaceCustody, ForkWorkspacePolicy, ForkWorkspaceSeed, PreparedForkWorkspace,
};
use exomonad_tool::{ToolArguments, ToolInvocation};
use tidepool_bridge_effects::{
    WtBranchName, WtGitOid, WtWorktreeHandle, WtWorktreeId, WtWorktreeReceipt,
};
use tidepool_runtime::session::{
    insert_preamble_imports, ModuleEnv, SessionLib, WorkbenchForkBoundary,
};
use tidepool_testing::eval_harness;

struct PausedWorkspace {
    entered: mpsc::UnboundedSender<String>,
    release: tokio::sync::Semaphore,
    reject: std::sync::atomic::AtomicBool,
}

struct WorkspaceCustody;
impl ForkWorkspaceCustody for WorkspaceCustody {
    fn actor_stopped(&self, _: &ActorTerminal) {}
    fn process_may_exist(&self) {}
}

impl ForkWorkspaceAdmission for PausedWorkspace {
    fn install_custody(
        &self,
        _: ActorRef,
        _: &str,
        _: crate::ActorRole,
    ) -> Result<Arc<dyn ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        Ok(Arc::new(WorkspaceCustody))
    }

    fn admit(
        &self,
        _: ActorRef,
        actor_path: String,
        _: ForkWorkspaceSeed,
        _: ForkWorkspacePolicy,
    ) -> ForkWorkspaceAdmissionFuture<'_> {
        Box::pin(async move {
            self.entered
                .send(actor_path.clone())
                .expect("fixture observes actual workspace admission");
            self.release
                .acquire()
                .await
                .expect("workspace release remains live")
                .forget();
            if self.reject.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(ForkWorkspaceAdmissionError {
                    detail: "controlled parent continuation failure".into(),
                });
            }
            Ok(PreparedForkWorkspace::new(
                WtWorktreeHandle {
                    handle_receipt: WtWorktreeReceipt {
                        tree_id: WtWorktreeId { raw: actor_path },
                        cwd: "/fixture-checkpoint-workspace".into(),
                        branch: WtBranchName {
                            raw: "checkpoint-readers".into(),
                        },
                        source_head: WtGitOid {
                            raw: "0123456789012345678901234567890123456789".into(),
                        },
                        snapshot_ref: None,
                        created_at: 0,
                    },
                },
                |_| Ok(Arc::new(WorkspaceCustody)),
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
    crate::ResidentInteractivePolicy::local(actor)
        .dispatch_boxed(ToolInvocation {
            context,
            name: crate::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(text),
        })
        .await
        .expect("production Haskell endpoint succeeds")
        .into_json()
        .expect("resident response serializes for this structured test helper")
}

fn assert_committed(reply: &serde_json::Value) {
    assert_eq!(reply["status"], "committed", "{reply:?}");
}

#[tokio::test]
async fn two_checkpoint_children_remint_after_workspace_wait_token_release_and_issuer_failure() {
    eval_harness::require_extract();
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::forks_decl(),
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
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Effects.Core as Core");
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Actor as Mailbox");
    let preamble = format!(
        "{preamble}\ndata CaptureProtocol result where CaptureNoop :: CaptureProtocol ()\ndata CaptureRead = CaptureRead deriving (Generic, FromJSON, JsonSchema)\ndata CaptureTools mode = CaptureTools {{ ping :: mode :- Call CaptureRead Int }} deriving Generic\n"
    );
    let root = tempfile::tempdir().expect("session root");
    let session = tidepool_repr::SessionId(std::process::id() as u64 * 10_000 + 184);
    let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .expect("declaration plane")
        .with_validation_include(include.clone());
    let machine = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let (entered, mut entered_rx) = mpsc::unbounded_channel();
    let workspaces = Arc::new(PausedWorkspace {
        entered,
        release: tokio::sync::Semaphore::new(0),
        reject: std::sync::atomic::AtomicBool::new(false),
    });
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        Some(workspaces.clone()),
        crate::Incarnation::FIRST,
    );
    let role = crate::ActorCapabilities::default().with_effect_keys(vec![crate::ActorEffectKey::Forks]);
    let issuer = forest
        .new_workbench("checkpoint-issuer".into(), role.clone())
        .await
        .expect("issuer");
    assert_committed(&run_cell(issuer.clone(), "let capturedValue = 41 :: Int".into()).await);
    let original_interface = root
        .path()
        .join(tidepool_repr::SessionModule::val(tidepool_repr::Generation(1)).relative_hi_path());
    eprintln!(
        "checkpoint workspace fixture: original interface {} observation {:?}",
        original_interface.display(),
        std::fs::read(&original_interface).map(|bytes| (bytes.len(), blake3::hash(&bytes)))
    );
    let issuer_context = forest
        .directory
        .session_context(issuer.identity())
        .expect("real issuer context");
    let (captured_scope, retained_scope) = forest
        .environment
        .runner
        .capture_retained_context_scope(issuer_context.clone())
        .await
        .expect("captured native lexical share");
    let token = forest
        .environment
        .fork_groups
        .capture_checkpoint_with_retained_scope(
            "workspace-delayed".into(),
            issuer.identity(),
            role.clone(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            session,
            captured_scope,
            WorkbenchForkBoundary::external(
                "local-fixture".into(),
                "request".into(),
                "captured-issuer".into(),
            ),
            None,
            retained_scope,
            crate::ActorPersistencePolicy::Ephemeral,
        );
    forest
        .environment
        .fork_groups
        .settle_checkpoint(&token, session, true)
        .expect("capture published");

    let mut calls = Vec::new();
    let launcher = forest
        .new_workbench("shared-launcher".into(), role.clone())
        .await
        .expect("launcher");
    for label in ["first", "second"] {
        let invocation = exomonad_tool::ToolInvocationContext::external(
            "capture-fixture".into(),
            label.into(),
            "haskell".into(),
            None,
            None,
        );
        let execution = crate::resident_tools::execution_id(
            launcher.identity(),
            &crate::resident_tools::WorkbenchCallKey::from(invocation.clone()),
        );
        let boundary = WorkbenchForkBoundary::Execution {
            actor_id: launcher.identity().id.0,
            incarnation: launcher.identity().incarnation.0,
            execution_id: execution,
        };
        let group_path = crate::ActorPath::parse(&format!("late/{label}")).expect("group path");
        let (group, reservations) = forest
            .environment
            .fork_groups
            .begin_at_boundary(
                launcher.identity(),
                group_path,
                vec![crate::ActorPathSegment::new("reader").unwrap()],
                None,
                boundary,
            )
            .expect("owning group admission");
        let source = include_str!("capture_workspace_child.hs")
            .replace("CHILD_PATH", &reservations[0].allocated.to_string())
            .replace("GROUP_ID", &group.0.to_string())
            .replace("CHECKPOINT_TOKEN", &token);
        calls.push(tokio::spawn(run_cell_with_context(
            launcher.clone(),
            source,
            Some(invocation),
        )));
    }
    let mut admitted_paths = Vec::new();
    for _ in 0..2 {
        admitted_paths.push(
            tokio::time::timeout(std::time::Duration::from_secs(240), async {
                loop {
                    if let Some(index) = calls.iter().position(|call| call.is_finished()) {
                        panic!(
                            "fork caller settled before workspace admission: {:?}",
                            calls.remove(index).await
                        );
                    }
                    tokio::select! {
                        path = entered_rx.recv() => return path.expect("workspace request"),
                        () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {},
                    }
                }
            })
            .await
            .expect("real fork reaches paused workspace admission"),
        );
    }
    eprintln!("checkpoint workspace fixture: both launch workspace admissions are waiting");
    assert_ne!(admitted_paths[0], admitted_paths[1]);
    assert!(calls.iter().all(|call| !call.is_finished()));
    let progressing = tokio::time::timeout(
        std::time::Duration::from_secs(240),
        run_cell(launcher.clone(), "pure (42 :: Int)".into()),
    )
    .await
    .expect("third cell progresses while both launches wait");
    assert_committed(&progressing);
    eprintln!("checkpoint workspace fixture: independent third cell committed");
    assert!(calls.iter().all(|call| !call.is_finished()));
    let retired = forest
        .environment
        .fork_groups
        .release_checkpoint(&token, session)
        .expect("issuer token release")
        .expect("original captured root retires");
    assert_eq!(retired, captured_scope);
    forest
        .environment
        .runner
        .retire_checkpoint_scopes(session, vec![retired])
        .await
        .expect("original root retirement acknowledged during both workspace waits");
    forest
        .environment
        .fork_groups
        .confirm_checkpoint_release(&token, session, retired)
        .expect("release confirmed");
    issuer
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "issuer failed after admission".into(),
            diagnostic: None,
        })
        .await
        .expect("actual issuer failure and root cleanup");
    assert_eq!(issuer.terminal().wait().await.kind, ActorExitKind::Failed);
    eprintln!("checkpoint workspace fixture: original checkpoint scope retired and issuer failed");
    workspaces.release.add_permits(2);
    for call in calls {
        assert_committed(
            &tokio::time::timeout(std::time::Duration::from_secs(240), call)
                .await
                .expect("admitted child remints after original root loss")
                .expect("launch caller task"),
        );
    }

    eprintln!("checkpoint workspace fixture: both launch cells committed their groups");
    eprintln!(
        "checkpoint workspace fixture: startup interface {} observation {:?}",
        original_interface.display(),
        std::fs::read(&original_interface).map(|bytes| (bytes.len(), blake3::hash(&bytes)))
    );
    let children = await_capture_reader_policies(
        &forest,
        &mut deployments,
        &admitted_paths,
        "remint-after-issuer-loss",
    )
    .await;
    eprintln!("checkpoint workspace fixture: both actual child policies installed");
    assert_ne!(children[0].actor.identity(), children[1].actor.identity());
    for child in &children {
        let reply = child
            .policy
            .dispatch_boxed(ToolInvocation {
                context: None,
                name: crate::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw("pure (capturedValue + 1)".into()),
            })
            .await
            .expect("child workbench reads the original issuer checkpoint binding");
        let reply = reply
            .into_json()
            .expect("resident response serializes for structured receipt assertions");
        assert_committed(&reply);
        assert_eq!(
            reply["items"]
                .as_array()
                .expect("item receipts")
                .last()
                .expect("expression receipt")["output"]
                .as_str()
                .expect("rendered value")
                .trim(),
            "42",
            "{reply:?}"
        );
    }
    for child in children {
        let result = child
            .actor
            .shutdown_with_cleanup(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "workspace capture fixture done".into(),
                diagnostic: None,
            })
            .await
            .expect("child shutdown");
        assert!(result.cleanup.is_confirmed(), "{:?}", result.cleanup);
    }
    drop(launcher);
    forest.shutdown().await;
    assert_eq!(
        forest
            .measurement_snapshot()
            .expect("native session remains shared")
            .parked,
        Some(0)
    );
}

#[tokio::test]
async fn two_captured_readers_reply_before_parent_failure_and_survive_final_checkpoint_release() {
    eval_harness::require_extract();
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::forks_decl(),
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
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Effects.Core as Core");
    let preamble = format!(
        "{preamble}\ndata CaptureRead = CaptureRead deriving (Generic, FromJSON, JsonSchema)\ndata CaptureTools mode = CaptureTools {{ ping :: mode :- Call CaptureRead Int }} deriving Generic\n"
    );
    let root = tempfile::tempdir().expect("session root");
    let session = tidepool_repr::SessionId(std::process::id() as u64 * 10_000 + 185);
    let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .expect("declaration plane")
        .with_validation_include(include.clone());
    let machine = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let (entered, mut entered_rx) = mpsc::unbounded_channel();
    let workspaces = Arc::new(PausedWorkspace {
        entered,
        release: tokio::sync::Semaphore::new(2),
        reject: std::sync::atomic::AtomicBool::new(false),
    });
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        Some(workspaces.clone()),
        crate::Incarnation::FIRST,
    );
    let role = crate::ActorCapabilities::default().with_effect_keys(vec![crate::ActorEffectKey::Forks]);
    let parent = forest
        .new_workbench("private-capture-parent".into(), role.clone())
        .await
        .expect("parent");
    assert_committed(&run_cell(parent.clone(), "let capturedValue = 41 :: Int".into()).await);
    let context = forest
        .directory
        .session_context(parent.identity())
        .expect("parent context");
    let (scope, retained) = forest
        .environment
        .runner
        .capture_retained_context_scope(context)
        .await
        .expect("real native private capture");
    let token = forest
        .environment
        .fork_groups
        .capture_checkpoint_with_retained_scope(
            "completed-private".into(),
            parent.identity(),
            role,
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            session,
            scope,
            WorkbenchForkBoundary::external(
                "native-capture".into(),
                "capture-request".into(),
                "completed-capture".into(),
            ),
            Some(HostedCheckpointAttachment::captured(Arc::new(()))),
            retained,
            crate::ActorPersistencePolicy::Ephemeral,
        );
    forest
        .environment
        .fork_groups
        .settle_checkpoint(&token, session, true)
        .expect("private capture completed");
    eprintln!("captured reader fixture: original private capture is published");
    let source = include_str!("captured_readers_parent.hs").replace("CHECKPOINT_TOKEN", &token);
    let endpoint = crate::ResidentInteractivePolicy::local(parent.clone());
    let call = tokio::spawn(async move {
        endpoint
            .dispatch_boxed(ToolInvocation {
                context: None,
                name: crate::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(source),
            })
            .await
    });
    let mut children = Vec::new();
    while children.len() < 2 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(240), deployments.recv())
            .await
            .expect("real captured child starts before parent returns")
            .expect("deployment stream");
        if let LocalResidentDeployment::PolicyInstalled(installation) = event {
            children.push(installation);
        }
        assert!(
            !call.is_finished(),
            "parent settled before captured publication/readers"
        );
    }
    assert_ne!(children[0].actor.identity(), children[1].actor.identity());
    let group = children[0].fork_group.expect("captured group identity");
    assert_eq!(children[1].fork_group, Some(group));
    let gate = forest
        .environment
        .fork_groups
        .gate(group, children[0].actor.identity())
        .expect("original group gate");
    tokio::time::timeout(std::time::Duration::from_secs(240), gate.wait_committed())
        .await
        .expect("captured commit completes while parent remains parked")
        .expect("actual publication");
    assert_eq!(
        gate.publication().expect("published group"),
        crate::ForkGroupPublication::Captured
    );
    eprintln!("captured reader fixture: real captured group committed before parent completion");
    for child in &children {
        assert_captured_reader(child).await;
        assert!(
            !call.is_finished(),
            "both child replies must precede parent completion"
        );
    }
    let blocker = tokio::time::timeout(std::time::Duration::from_secs(240), async {
        loop {
            let path = entered_rx.recv().await.expect("workspace request");
            if path.contains("failure-barrier") {
                break path;
            }
        }
    })
    .await
    .expect("parent continues into the controlled external wait after captured publication");
    assert!(blocker.contains("blocker"));
    assert!(!call.is_finished());
    workspaces
        .reject
        .store(true, std::sync::atomic::Ordering::SeqCst);
    workspaces.release.add_permits(1);
    let parent_reply = tokio::time::timeout(std::time::Duration::from_secs(30), call)
        .await
        .expect("failed parent call settles")
        .expect("caller task");
    let failure =
        parent_reply.expect_err("the continued parent cell raises the controlled launch refusal");
    assert!(
        format!("{failure:?}").contains("controlled parent continuation failure"),
        "{failure:?}"
    );
    assert!(
        parent.terminal().get().is_none(),
        "this gate fails the parent cell while its supervising actor remains live"
    );
    forest
        .environment
        .fork_groups
        .checkpoint(&token, session)
        .expect("completed private capture admits readers after parent cell failure");
    for child in &children {
        assert_captured_reader(child).await;
    }
    workspaces
        .reject
        .store(false, std::sync::atomic::Ordering::SeqCst);
    workspaces.release.add_permits(1);
    eprintln!("captured reader fixture: parent cell failed, supervising actor remains live");
    let reused = include_str!("captured_reader_reuse.hs").replace("CHECKPOINT_TOKEN", &token);
    assert_committed(&run_cell(parent.clone(), reused).await);
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(240), deployments.recv())
            .await
            .expect("a fresh reader uses the same published capture after parent cell failure")
            .expect("deployment stream");
        if let LocalResidentDeployment::PolicyInstalled(installation) = event {
            assert_captured_reader(&installation).await;
            children.push(installation);
            break;
        }
    }
    eprintln!("captured reader fixture: fresh reader reused the original published capture");
    let retired = forest
        .environment
        .fork_groups
        .release_checkpoint(&token, session)
        .expect("private capture release")
        .expect("last checkpoint token retires original root");
    assert_eq!(retired, scope);
    forest
        .environment
        .runner
        .retire_checkpoint_scopes(session, vec![scope])
        .await
        .expect("capture original root cleanup");
    forest
        .environment
        .fork_groups
        .confirm_checkpoint_release(&token, session, scope)
        .expect("original root release confirmed");
    for child in &children {
        assert_captured_reader(child).await;
    }
    let first = children.remove(0);
    let result = first
        .actor
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "first captured reader done".into(),
            diagnostic: None,
        })
        .await
        .expect("first reader shutdown");
    assert!(result.cleanup.is_confirmed(), "{:?}", result.cleanup);
    assert_captured_reader(&children[0]).await;
    let second = children.remove(0);
    let result = second
        .actor
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "second captured reader done".into(),
            diagnostic: None,
        })
        .await
        .expect("second reader shutdown");
    assert!(result.cleanup.is_confirmed(), "{:?}", result.cleanup);
    assert_captured_reader(&children[0]).await;
    let final_reader = children.remove(0);
    let result = final_reader
        .actor
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "final captured reader done".into(),
            diagnostic: None,
        })
        .await
        .expect("final reader shutdown");
    assert!(result.cleanup.is_confirmed(), "{:?}", result.cleanup);
    forest.shutdown().await;
    assert_eq!(
        forest
            .measurement_snapshot()
            .expect("shared native session")
            .parked,
        Some(0)
    );
}

async fn await_capture_reader_policies(
    forest: &ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>,
    deployments: &mut mpsc::Receiver<LocalResidentDeployment>,
    paths: &[String],
    stage: &str,
) -> Vec<Box<LocalResidentInstallation>> {
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(240));
    tokio::pin!(deadline);
    let mut observe = tokio::time::interval(std::time::Duration::from_secs(5));
    let mut children = Vec::new();
    let mut observed = Vec::new();
    while children.len() < paths.len() {
        let records = forest
            .environment
            .actors
            .lock()
            .iter()
            .filter(|(_, record)| {
                record
                    .descriptor
                    .actor_path()
                    .is_some_and(|path| paths.contains(&path.to_string()))
            })
            .map(|(actor, record)| {
                (
                    *actor,
                    record.terminal.clone(),
                    record.interactive_policy_installed,
                )
            })
            .collect::<Vec<_>>();
        for (actor, terminal, _) in &records {
            let terminal = terminal.clone().or_else(|| {
                forest
                    .directory
                    .resolve(*actor)
                    .and_then(|child| child.terminal().get())
            });
            assert!(terminal.is_none(), "capture readiness stage={stage} actor={actor:?} ended before all policies installed: {terminal:?}; events={observed:?}");
        }
        tokio::select! {
            event = deployments.recv() => {
                let event = event.expect("capture deployment observer");
                observed.push(event.kind());
                match event {
                    LocalResidentDeployment::PolicyInstalled(child) => {
                        let actor = child.actor.identity();
                        assert!(records.iter().any(|(expected, _, _)| *expected == actor),
                            "unrelated policy installed during capture readiness: {actor:?}");
                        assert!(!children.iter().any(|installed: &Box<LocalResidentInstallation>| installed.actor.identity() == actor), "duplicate policy installation");
                        children.push(child);
                    }
                    LocalResidentDeployment::ChildExited { notice }
                        if records.iter().any(|(actor, _, _)| *actor == notice.child.identity()) => {
                        panic!("capture readiness stage={stage} child exited: {notice:?}; events={observed:?}");
                    }
                    LocalResidentDeployment::Retired { actor, terminal }
                        if records.iter().any(|(expected, _, _)| *expected == actor) => {
                        panic!("capture readiness stage={stage} child retired {actor:?}: {terminal:?}; events={observed:?}");
                    }
                    _ => {}
                }
            }
            _ = observe.tick() => eprintln!("capture readiness stage={stage} records={records:?}; installed={}; events={observed:?}", children.len()),
            _ = &mut deadline => panic!("capture readiness stage={stage} did not complete: records={records:?}; events={observed:?}; graph={:?}", forest.inspect_host_graph()),
        }
    }
    children
}

async fn await_capture_reader_policy(
    forest: &ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>,
    deployments: &mut mpsc::Receiver<LocalResidentDeployment>,
    label: &str,
    stage: &str,
) -> Box<LocalResidentInstallation> {
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(240));
    tokio::pin!(deadline);
    let mut observe = tokio::time::interval(std::time::Duration::from_secs(5));
    let mut observed = Vec::new();
    let mut identity = None;
    loop {
        let record = forest
            .environment
            .actors
            .lock()
            .iter()
            .find(|(_, record)| record.descriptor.label() == label)
            .map(|(actor, record)| {
                (
                    *actor,
                    record.terminal.clone(),
                    record.interactive_policy_installed,
                )
            });
        if let Some((actor, terminal, _)) = &record {
            identity = Some(*actor);
            let terminal = terminal.clone().or_else(|| {
                forest
                    .directory
                    .resolve(*actor)
                    .and_then(|child| child.terminal().get())
            });
            assert!(terminal.is_none(),
                "capture readiness stage={stage} label={label} actor={actor:?} ended before policy installation: terminal={terminal:?}; events={observed:?}; graph={:?}",
                forest.inspect_host_graph());
        }
        tokio::select! {
            event = deployments.recv() => {
                let event = event.unwrap_or_else(|| panic!(
                    "capture readiness stage={stage} deployment stream closed; events={observed:?}"));
                observed.push(event.kind());
                match event {
                    LocalResidentDeployment::PolicyInstalled(child) if child.label == label => {
                        eprintln!("capture readiness stage={stage} installed actor={:?}; events={observed:?}", child.actor.identity());
                        return child;
                    }
                    LocalResidentDeployment::ChildExited { notice }
                        if Some(notice.child.identity()) == identity => {
                        panic!("capture readiness stage={stage} child exited: {notice:?}; events={observed:?}; graph={:?}", forest.inspect_host_graph());
                    }
                    LocalResidentDeployment::Retired { actor, terminal }
                        if Some(actor) == identity => {
                        panic!("capture readiness stage={stage} child retired actor={actor:?}: {terminal:?}; events={observed:?}; graph={:?}", forest.inspect_host_graph());
                    }
                    other => {
                        eprintln!("capture readiness stage={stage} observed deployment={}; target={identity:?}", other.kind());
                    }
                }
            }
            _ = observe.tick() => {
                eprintln!("capture readiness stage={stage} target={record:?}; events={observed:?}; graph={:?}", forest.inspect_host_graph());
            }
            _ = &mut deadline => panic!(
                "capture readiness stage={stage} timed out label={label}; target={record:?}; events={observed:?}; graph={:?}", forest.inspect_host_graph()),
        }
    }
}

#[tokio::test]
async fn partial_captured_group_startup_failure_cleans_first_child_and_preserves_capture() {
    eval_harness::require_extract();
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::forks_decl(),
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
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Effects.Core as Core");
    let preamble = format!(
        "{preamble}\ndata CaptureRead = CaptureRead deriving (Generic, FromJSON, JsonSchema)\ndata CaptureTools mode = CaptureTools {{ ping :: mode :- Call CaptureRead Int }} deriving Generic\n"
    );
    let root = tempfile::tempdir().expect("session root");
    let session = tidepool_repr::SessionId(u64::from(std::process::id()) * 10_000 + 186);
    let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .expect("declaration plane")
        .with_validation_include(include.clone());
    let machine = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let (entered, mut entered_rx) = mpsc::unbounded_channel();
    let workspaces = Arc::new(PausedWorkspace {
        entered,
        release: tokio::sync::Semaphore::new(1),
        reject: std::sync::atomic::AtomicBool::new(false),
    });
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        Some(workspaces.clone()),
        crate::Incarnation::FIRST,
    );
    let role = crate::ActorCapabilities::default().with_effect_keys(vec![crate::ActorEffectKey::Forks]);
    let parent = forest
        .new_workbench("partial-capture-parent".into(), role.clone())
        .await
        .expect("parent");
    assert_committed(&run_cell(parent.clone(), "let capturedValue = 41 :: Int".into()).await);
    let context = forest
        .directory
        .session_context(parent.identity())
        .expect("parent context");
    let (scope, retained) = forest
        .environment
        .runner
        .capture_retained_context_scope(context)
        .await
        .expect("real native capture");
    let token = forest
        .environment
        .fork_groups
        .capture_checkpoint_with_retained_scope(
            "completed-before-partial-start".into(),
            parent.identity(),
            role,
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            session,
            scope,
            WorkbenchForkBoundary::external(
                "native-capture".into(),
                "capture-request".into(),
                "completed-capture".into(),
            ),
            Some(HostedCheckpointAttachment::captured(Arc::new(()))),
            retained,
            crate::ActorPersistencePolicy::Ephemeral,
        );
    forest
        .environment
        .fork_groups
        .settle_checkpoint(&token, session, true)
        .expect("independently completed capture");

    let reuse = include_str!("captured_reader_reuse.hs").replace("CHECKPOINT_TOKEN", &token);
    assert_committed(&run_cell(parent.clone(), reuse.clone()).await);
    let survivor_path = entered_rx
        .recv()
        .await
        .expect("survivor workspace admission");
    assert!(survivor_path.contains("captured/reused"));
    let survivor = await_capture_reader_policy(
        &forest,
        &mut deployments,
        &survivor_path,
        "independent reader before partial startup",
    )
    .await;
    assert_captured_reader(&survivor).await;
    assert_eq!(
        survivor
            .fork_gate
            .as_ref()
            .expect("survivor group gate")
            .publication()
            .unwrap(),
        crate::ForkGroupPublication::Captured
    );

    workspaces.release.add_permits(1);
    let source =
        include_str!("capture_partial_startup_failure.hs").replace("CHECKPOINT_TOKEN", &token);
    let endpoint = crate::ResidentInteractivePolicy::local(parent.clone());
    let mut call = tokio::spawn(async move {
        endpoint
            .dispatch_boxed(ToolInvocation {
                context: None,
                name: crate::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(source),
            })
            .await
    });
    let paths = tokio::time::timeout(std::time::Duration::from_secs(240), async {
        let mut paths = Vec::new();
        while paths.len() < 2 {
            tokio::select! {
                path = entered_rx.recv() => paths.push(path.expect("partial workspace admission")),
                reply = &mut call => panic!("partial caller settled before the second workspace wait: {reply:?}"),
            }
        }
        paths
    }).await.expect("first child starts before second workspace blocks");
    assert_ne!(paths[0], paths[1]);
    assert!(paths
        .iter()
        .all(|path| path.contains("captured/partial-startup")));
    let (first, group) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let recorded = forest
                .environment
                .actors
                .lock()
                .iter()
                .find(|(_, record)| record.descriptor.label() == paths[0])
                .map(|(actor, record)| {
                    (
                        *actor,
                        record.descriptor.fork_group().expect("partial group"),
                    )
                });
            if let Some((identity, group)) = recorded {
                if let Some(actor) = forest.directory.resolve(identity) {
                    break (actor, group);
                }
            }
            assert!(!call.is_finished(), "second workspace remains held");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual first child is registered");
    assert!(first.terminal().get().is_none());
    assert_ne!(first.identity(), survivor.actor.identity());
    assert_eq!(
        forest
            .environment
            .fork_groups
            .children_for_owner(group, parent.identity())
            .unwrap(),
        vec![first.identity()]
    );
    let gate = forest
        .environment
        .fork_groups
        .gate(group, first.identity())
        .expect("already attached first child");
    assert!(
        matches!(gate.publication(), Err(crate::lineage::ForkGroupError::NotCommitted(id)) if id == group.0)
    );
    eprintln!("partial capture fixture: first child {:?} exists; second workspace held; group unpublished", first.identity());

    workspaces
        .reject
        .store(true, std::sync::atomic::Ordering::SeqCst);
    workspaces.release.add_permits(1);
    let failure = tokio::time::timeout(std::time::Duration::from_secs(30), call)
        .await
        .expect("partial refusal settles parent cell")
        .expect("caller task")
        .expect_err("second startup failure must fail this cell");
    let detail = format!("{failure:?}");
    assert!(
        detail.contains("expected partial child startup refusal")
            && detail.contains("controlled parent continuation failure"),
        "{detail}"
    );
    assert!(
        parent.terminal().get().is_none(),
        "cell failure leaves its supervising actor live"
    );
    assert!(
        gate.wait_committed().await.is_err(),
        "failed group must never publish"
    );
    assert!(forest
        .environment
        .fork_groups
        .children_for_owner(group, parent.identity())
        .is_err());
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(30), first.terminal().wait())
            .await
            .expect("first child actually retires")
            .kind,
        ActorExitKind::Cancelled
    );
    let cleanup = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            // An unconsumed installation is observer custody, not live actor work.
            while let Ok(event) = deployments.try_recv() {
                drop(event);
            }
            if let Some(cleanup) = first.terminal().cleanup() {
                break cleanup;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first child cleanup is retained");
    assert!(cleanup.is_confirmed(), "{cleanup:?}");
    forest
        .environment
        .fork_groups
        .checkpoint(&token, session)
        .expect("group abort does not release completed capture");
    assert!(
        survivor.actor.terminal().get().is_none(),
        "unrelated published captured child survives"
    );
    assert_captured_reader(&survivor).await;
    eprintln!("partial capture fixture: failed first child cleanup confirmed; original capture and published reader survive");

    workspaces
        .reject
        .store(false, std::sync::atomic::Ordering::SeqCst);
    workspaces.release.add_permits(1);
    assert_committed(&run_cell(parent.clone(), reuse).await);
    let fresh_path = tokio::time::timeout(std::time::Duration::from_secs(240), entered_rx.recv())
        .await
        .expect("fresh reader workspace admission settles")
        .expect("fresh reader workspace admission");
    let fresh = await_capture_reader_policy(
        &forest,
        &mut deployments,
        &fresh_path,
        "fresh reader after failed group cleanup",
    )
    .await;
    assert_ne!(fresh.actor.identity(), first.identity());
    assert_ne!(fresh.actor.identity(), survivor.actor.identity());
    assert_captured_reader(&fresh).await;
    let retired = forest
        .environment
        .fork_groups
        .release_checkpoint(&token, session)
        .unwrap()
        .expect("original capture root");
    assert_eq!(retired, scope);
    forest
        .environment
        .runner
        .retire_checkpoint_scopes(session, vec![scope])
        .await
        .expect("original scope cleanup");
    forest
        .environment
        .fork_groups
        .confirm_checkpoint_release(&token, session, scope)
        .unwrap();
    assert_captured_reader(&survivor).await;
    assert_captured_reader(&fresh).await;
    let stopped = survivor
        .actor
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "surviving reader done".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    assert!(stopped.cleanup.is_confirmed(), "{:?}", stopped.cleanup);
    assert_captured_reader(&fresh).await;
    let stopped = fresh
        .actor
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "fresh reader done".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    assert!(stopped.cleanup.is_confirmed(), "{:?}", stopped.cleanup);
    forest.shutdown().await;
    assert_eq!(
        forest
            .measurement_snapshot()
            .expect("shared native session")
            .parked,
        Some(0)
    );
}

async fn assert_captured_reader(child: &LocalResidentInstallation) {
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(240),
        child.policy.dispatch_boxed(ToolInvocation {
            context: None,
            name: "ping".into(),
            // The authored CaptureRead product decodes the empty argument object.
            arguments: ToolArguments::Structured(serde_json::json!({})),
        }),
    )
    .await
    .expect("captured reader reply bounded")
    .expect("real captured native tool reply");
    assert!(
        matches!(&reply, crate::ResidentToolResponse::Value(value) if *value == serde_json::json!(42)),
        "{reply:?}"
    );
}
