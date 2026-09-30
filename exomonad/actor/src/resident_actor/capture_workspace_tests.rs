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
    crate::ResidentInteractivePolicy::local(actor)
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: crate::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(text),
        })
        .await
        .expect("production Haskell endpoint succeeds")
}

fn assert_committed(reply: &serde_json::Value) {
    assert_eq!(reply["status"], "committed", "{reply:?}");
}

#[tokio::test]
async fn two_checkpoint_children_remint_after_workspace_wait_token_release_and_issuer_failure() {
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
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Effects.Core as Core");
    let preamble = format!("{preamble}\ndata CaptureTools mode = CaptureTools {{ ping :: mode :- Call () Int }} deriving Generic\n");
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
    });
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        Some(workspaces.clone()),
        crate::Incarnation::FIRST,
    );
    let role = crate::EffectiveRole::root().with_effect_keys(vec![crate::ActorEffectKey::Forks]);
    let issuer = forest
        .new_workbench("checkpoint-issuer".into(), role.clone())
        .await
        .expect("issuer");
    assert_committed(&run_cell(issuer.clone(), "let capturedValue = 41 :: Int".into()).await);
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
            WorkbenchForkBoundary {
                thread_id: "local-fixture".into(),
                call_id: "captured-issuer".into(),
            },
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
    let mut launchers = Vec::new();
    for label in ["first", "second"] {
        let launcher = forest
            .new_workbench(format!("launcher-{label}"), role.clone())
            .await
            .expect("launcher");
        let group_path = crate::ActorPath::parse(&format!("late/{label}")).expect("group path");
        let (group, reservations) = forest
            .environment
            .fork_groups
            .begin(
                launcher.identity(),
                group_path,
                vec![crate::ActorPathSegment::new("reader").unwrap()],
                None,
            )
            .expect("owning group admission");
        let source = include_str!("capture_workspace_child.hs")
            .replace("CHILD_PATH", &reservations[0].allocated.to_string())
            .replace("GROUP_ID", &group.0.to_string())
            .replace("CHECKPOINT_TOKEN", &token);
        calls.push(tokio::spawn(run_cell(launcher.clone(), source)));
        launchers.push(launcher);
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
    assert_ne!(admitted_paths[0], admitted_paths[1]);
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
        })
        .await
        .expect("actual issuer failure and root cleanup");
    assert_eq!(issuer.terminal().wait().await.kind, ActorExitKind::Failed);
    workspaces.release.add_permits(2);
    for call in calls {
        assert_committed(
            &tokio::time::timeout(std::time::Duration::from_secs(240), call)
                .await
                .expect("admitted child remints after original root loss")
                .expect("launch caller task"),
        );
    }

    let mut children = Vec::new();
    while children.len() < 2 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(30), deployments.recv())
            .await
            .expect("child policy startup is bounded")
            .expect("deployment observer");
        if let LocalResidentDeployment::PolicyInstalled(installation) = event {
            children.push(installation);
        }
    }
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
            .expect("real child evaluates inherited lexical binding");
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
            })
            .await
            .expect("child shutdown");
        assert!(result.cleanup.is_confirmed(), "{:?}", result.cleanup);
    }
    drop(launchers);
    forest.shutdown().await;
    assert_eq!(
        forest
            .measurement_snapshot()
            .expect("native session remains shared")
            .parked,
        Some(0)
    );
}
