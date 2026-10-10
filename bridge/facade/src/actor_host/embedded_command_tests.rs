use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use exomonad_actor::{ActorGraphNode, ActorId, ActorRef, ActorWorkbenchPosture, Incarnation};
use harness::{
    embedding::{
        AdmissionGuard, ClientOperationId, Conversation, EmbeddedError, EmbeddedRoundId, HostActor,
        HostControl, HostControlError, HostIdentity,
    },
    model::AgentPath,
    server::{ClientCommand, CommandControl, CommandReceiptOutcome, HostCommand},
    store::Store,
};
use serde_json::Value;

use super::*;

struct TestLease;

impl AdmissionGuard for TestLease {}

struct TestHost {
    identity: HostIdentity,
    wakes: AtomicUsize,
    admissions: AtomicUsize,
    uncertain_retire: std::sync::atomic::AtomicBool,
    controls: std::sync::Mutex<Vec<HostControl>>,
    fail_retire: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl HostActor for TestHost {
    fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    fn admit(&self) -> Result<Box<dyn AdmissionGuard>, EmbeddedError> {
        self.admissions.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(TestLease))
    }

    fn tool_surface(&self) -> Result<Arc<harness::embedding::ToolSurface>, EmbeddedError> {
        Err(EmbeddedError::Surface("not used in routing test".into()))
    }

    async fn wake(&self, _envelope_id: i64) -> Result<(), String> {
        self.wakes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn control(&self, control: HostControl) -> Result<Value, HostControlError> {
        let refuse_retire =
            matches!(&control, HostControl::Retire) && self.fail_retire.load(Ordering::Relaxed);
        self.controls
            .lock()
            .expect("test host control mutex")
            .push(control);
        if self.uncertain_retire.load(Ordering::Relaxed) {
            Err(HostControlError::Unconfirmed(
                "retirement acknowledgment lost".into(),
            ))
        } else if refuse_retire {
            Err(HostControlError::Refused("retirement refused".into()))
        } else {
            Ok(serde_json::json!({ "requested": true }))
        }
    }
}

struct RoutingFixture {
    store: Arc<Store>,
    projection: embedded_projection::EmbeddedProjection,
    nodes: Vec<ActorGraphNode>,
    live: BTreeSet<ActorRef>,
    conversations: HashMap<ActorRef, Arc<Conversation>>,
    hosts: HashMap<ActorRef, Arc<TestHost>>,
    lifecycle: embedded_projection::LifecycleSender,
    lifecycle_rx: tokio::sync::watch::Receiver<embedded_projection::LifecycleState>,
}

impl RoutingFixture {
    fn new() -> Self {
        let store = Arc::new(Store::memory().expect("memory store"));
        let root_actor = actor(99);
        let first = actor(1);
        let second = actor(2);
        let root_identity = identity("run", "/root", root_actor);
        let first_identity = identity("run", "/root/1", first);
        let second_identity = identity("run", "/root/2", second);
        let root_host = Arc::new(TestHost {
            identity: root_identity.clone(),
            wakes: AtomicUsize::new(0),
            admissions: AtomicUsize::new(0),
            uncertain_retire: std::sync::atomic::AtomicBool::new(false),
            controls: std::sync::Mutex::new(Vec::new()),
            fail_retire: std::sync::atomic::AtomicBool::new(false),
        });
        let first_host = Arc::new(TestHost {
            identity: first_identity.clone(),
            wakes: AtomicUsize::new(0),
            admissions: AtomicUsize::new(0),
            uncertain_retire: std::sync::atomic::AtomicBool::new(false),
            controls: std::sync::Mutex::new(Vec::new()),
            fail_retire: std::sync::atomic::AtomicBool::new(false),
        });
        let second_host = Arc::new(TestHost {
            identity: second_identity.clone(),
            wakes: AtomicUsize::new(0),
            admissions: AtomicUsize::new(0),
            uncertain_retire: std::sync::atomic::AtomicBool::new(false),
            controls: std::sync::Mutex::new(Vec::new()),
            fail_retire: std::sync::atomic::AtomicBool::new(false),
        });
        let root = AgentPath("/root".into());
        let root_conversation = Arc::new(
            Conversation::attach(store.clone(), root_host.clone(), None).expect("attach root host"),
        );
        let first_conversation = Arc::new(
            Conversation::attach(store.clone(), first_host.clone(), Some(&root))
                .expect("attach first host"),
        );
        let second_conversation = Arc::new(
            Conversation::attach(store.clone(), second_host.clone(), Some(&root))
                .expect("attach second host"),
        );
        let mut projection = embedded_projection::EmbeddedProjection::default();
        projection.attached(root_actor, &root_identity);
        projection.attached(first, &first_identity);
        projection.attached(second, &second_identity);
        let (lifecycle, lifecycle_rx) = embedded_projection::LifecycleSender::channel();

        Self {
            store,
            projection,
            nodes: vec![
                model_node(root_actor),
                model_node(first),
                model_node(second),
            ],
            live: BTreeSet::from([root_actor, first, second]),
            conversations: HashMap::from([
                (root_actor, root_conversation),
                (first, first_conversation),
                (second, second_conversation),
            ]),
            hosts: HashMap::from([
                (root_actor, root_host),
                (first, first_host),
                (second, second_host),
            ]),
            lifecycle,
            lifecycle_rx,
        }
    }

    async fn dispatch(&self, _description: &str, command: ClientCommand) -> CommandReceiptOutcome {
        let ClientCommand::Host {
            operation_id,
            command,
        } = command
        else {
            panic!("routing fixtures require exact embedded host commands");
        };
        self.store
            .enqueue_embedded_command(operation_id, &command)
            .unwrap();
        dispatch_embedded_browser_command(
            operation_id,
            &command,
            &self.store,
            "run",
            &self.nodes,
            &self.live,
            &self.projection,
            |actor| self.conversations.get(&actor).cloned(),
            &self.lifecycle,
        )
        .await
        .unwrap()
        .expect("command settled")
        .outcome
    }
}

fn actor(id: u64) -> ActorRef {
    ActorRef {
        id: ActorId(id),
        incarnation: Incarnation::FIRST,
    }
}

fn new_operation() -> ClientOperationId {
    ClientOperationId(uuid::Uuid::new_v4())
}

fn identity(run: &str, path: &str, actor: ActorRef) -> HostIdentity {
    HostIdentity {
        run: run.into(),
        actor: AgentPath(path.into()),
        incarnation: actor.incarnation.0.to_string(),
    }
}

fn model_node(actor: ActorRef) -> ActorGraphNode {
    ActorGraphNode {
        actor,
        label: format!("actor-{}", actor.id.0),
        model_actor: true,
        creator: None,
        supervisor_parent: None,
        context_parent: None,
        terminal: None,
        workbench: ActorWorkbenchPosture::Idle,
        provider_thread: None,
        provider_turn: None,
        provider_observation_stale: false,
        bound_worktree: None,
        active_requests: Vec::new(),
        queued_requests: Vec::new(),
    }
}

#[tokio::test]
async fn production_dispatch_admits_input_only_to_the_exact_actor() {
    let fixture = RoutingFixture::new();
    let first = actor(1);
    let second = actor(2);
    let target = fixture.hosts[&second].identity.clone();

    let outcome = fixture
        .dispatch(
            "input-1",
            ClientCommand::Host {
                operation_id: new_operation(),
                command: HostCommand::Input {
                    target: target.clone(),
                    text: "hello child".into(),
                },
            },
        )
        .await;

    assert!(matches!(
        outcome,
        CommandReceiptOutcome::Admitted {
            target: Some(actual),
            envelope_id,
            wake_error: None,
        } if actual == target && !envelope_id.is_empty()
    ));
    assert_eq!(fixture.hosts[&first].wakes.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.hosts[&second].wakes.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn production_dispatch_refuses_wrong_run_and_incarnation_before_admission() {
    let fixture = RoutingFixture::new();
    let host = &fixture.hosts[&actor(2)];
    let identity = host.identity.clone();
    let mut wrong_run = identity.clone();
    wrong_run.run = "other-run".into();
    let mut wrong_incarnation = identity.clone();
    wrong_incarnation.incarnation = "2".into();

    for target in [wrong_run, wrong_incarnation] {
        let outcome = fixture
            .dispatch(
                "invalid-target",
                ClientCommand::Host {
                    operation_id: new_operation(),
                    command: HostCommand::Input {
                        target: target.clone(),
                        text: "must not wake".into(),
                    },
                },
            )
            .await;
        assert!(matches!(
            outcome,
            CommandReceiptOutcome::Refused {
                target: Some(actual),
                ..
            } if actual == target
        ));
    }
    assert_eq!(host.wakes.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn production_dispatch_routes_interrupt_and_retire_to_their_exact_actors() {
    let fixture = RoutingFixture::new();
    let first = actor(1);
    let second = actor(2);
    let first_identity = fixture.hosts[&first].identity.clone();
    let second_identity = fixture.hosts[&second].identity.clone();

    let interrupted = fixture
        .dispatch(
            "interrupt-2",
            ClientCommand::Host {
                operation_id: new_operation(),
                command: HostCommand::Interrupt {
                    expected_round: EmbeddedRoundId(uuid::Uuid::new_v4()),
                    target: second_identity.clone(),
                },
            },
        )
        .await;
    assert!(matches!(
        interrupted,
        CommandReceiptOutcome::ControlRequested {
            target,
            control: CommandControl::Interrupt,
        } if target == second_identity
    ));

    let retired = fixture
        .dispatch(
            "retire-1",
            ClientCommand::Host {
                operation_id: new_operation(),
                command: HostCommand::Retire {
                    target: first_identity.clone(),
                },
            },
        )
        .await;
    assert!(matches!(
        retired,
        CommandReceiptOutcome::ControlRequested {
            target,
            control: CommandControl::Retire,
        } if target == first_identity
    ));
    assert!(matches!(
        fixture.hosts[&first]
            .controls
            .lock()
            .expect("first controls")
            .as_slice(),
        [HostControl::Retire]
    ));
    assert!(matches!(
        fixture.hosts[&second]
            .controls
            .lock()
            .expect("second controls")
            .as_slice(),
        [HostControl::Interrupt { .. }]
    ));
    assert_eq!(
        fixture.lifecycle_rx.borrow()[&first],
        harness::server::HostActorLifecycle::Retiring
    );
    assert!(!fixture.lifecycle_rx.borrow().contains_key(&second));
}

#[tokio::test]
async fn refused_retirement_does_not_publish_retiring_lifecycle() {
    let fixture = RoutingFixture::new();
    let actor = actor(1);
    let target = fixture.hosts[&actor].identity.clone();
    fixture.hosts[&actor]
        .fail_retire
        .store(true, Ordering::Relaxed);

    let outcome = fixture
        .dispatch(
            "retire-refused",
            ClientCommand::Host {
                operation_id: new_operation(),
                command: HostCommand::Retire {
                    target: target.clone(),
                },
            },
        )
        .await;

    let CommandReceiptOutcome::Refused {
        target: Some(actual),
        reason,
    } = outcome
    else {
        panic!("expected a targeted retirement refusal");
    };
    assert_eq!(actual, target);
    assert!(
        reason == "retirement refused",
        "refusal should retain host error context: {reason}"
    );
    assert!(!fixture.lifecycle_rx.borrow().contains_key(&actor));
}

#[tokio::test]
async fn admitted_retry_after_retirement_does_not_resolve_admit_or_wake() {
    let mut fixture = RoutingFixture::new();
    let actor = actor(1);
    let operation_id = new_operation();
    let command = HostCommand::Input {
        target: fixture.hosts[&actor].identity.clone(),
        text: "retain this exact input".into(),
    };
    let first = fixture
        .dispatch(
            "first input",
            ClientCommand::Host {
                operation_id,
                command: command.clone(),
            },
        )
        .await;
    assert!(matches!(first, CommandReceiptOutcome::Admitted { .. }));
    let admissions = fixture.hosts[&actor].admissions.load(Ordering::Relaxed);
    assert_eq!(fixture.hosts[&actor].wakes.load(Ordering::Relaxed), 1);
    fixture.nodes.clear();
    fixture.live.clear();
    let duplicate = dispatch_embedded_browser_command(
        operation_id,
        &command,
        &fixture.store,
        "run",
        &fixture.nodes,
        &fixture.live,
        &fixture.projection,
        |_| panic!("retained retry must not resolve a live actor"),
        &fixture.lifecycle,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(duplicate.outcome, first);
    assert_eq!(
        fixture.hosts[&actor].admissions.load(Ordering::Relaxed),
        admissions
    );
    assert_eq!(fixture.hosts[&actor].wakes.load(Ordering::Relaxed), 1);
    let conflicting = HostCommand::Input {
        target: command.target().clone(),
        text: "changed payload".into(),
    };
    assert!(fixture
        .store
        .enqueue_embedded_command(operation_id, &conflicting)
        .is_err());
    assert_eq!(fixture.hosts[&actor].wakes.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn claimed_control_and_retained_uncertainty_never_redispatch() {
    let fixture = RoutingFixture::new();
    let actor = actor(1);
    let operation_id = new_operation();
    let command = HostCommand::Retire {
        target: fixture.hosts[&actor].identity.clone(),
    };
    fixture
        .store
        .enqueue_embedded_command(operation_id, &command)
        .unwrap();
    fixture
        .store
        .claim_embedded_command("run", operation_id)
        .unwrap()
        .unwrap();
    let dispatch = || {
        dispatch_embedded_browser_command(
            operation_id,
            &command,
            &fixture.store,
            "run",
            &fixture.nodes,
            &fixture.live,
            &fixture.projection,
            |_| panic!("claimed control must not resolve a live actor"),
            &fixture.lifecycle,
        )
    };
    assert!(dispatch().await.unwrap().is_none());
    let retained = fixture
        .store
        .settle_embedded_command(
            "run",
            operation_id,
            CommandReceiptOutcome::Unconfirmed {
                target: command.target().clone(),
                reason: "previous host lost after claim".into(),
            },
        )
        .unwrap();
    assert_eq!(dispatch().await.unwrap(), Some(retained));
    assert!(fixture.hosts[&actor].controls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_control_acknowledgment_is_retained_as_unconfirmed() {
    let fixture = RoutingFixture::new();
    let actor = actor(1);
    fixture.hosts[&actor]
        .uncertain_retire
        .store(true, Ordering::Relaxed);
    let operation_id = new_operation();
    let command = HostCommand::Retire {
        target: fixture.hosts[&actor].identity.clone(),
    };
    let first = fixture
        .dispatch(
            "uncertain retire",
            ClientCommand::Host {
                operation_id,
                command: command.clone(),
            },
        )
        .await;
    assert!(matches!(&first, CommandReceiptOutcome::Unconfirmed { .. }));
    assert_eq!(
        fixture
            .dispatch(
                "retry uncertain retire",
                ClientCommand::Host {
                    operation_id,
                    command,
                }
            )
            .await,
        first
    );
    assert_eq!(fixture.hosts[&actor].controls.lock().unwrap().len(), 1);
    assert!(!fixture.lifecycle_rx.borrow().contains_key(&actor));
}

#[tokio::test]
async fn durable_command_drain_runs_without_a_channel_hint_and_skips_claimed_work() {
    let fixture = RoutingFixture::new();
    let actor = actor(1);
    let operation_id = new_operation();
    let command = HostCommand::Input {
        target: fixture.hosts[&actor].identity.clone(),
        text: "persisted before wake".into(),
    };
    fixture
        .store
        .enqueue_embedded_command(operation_id, &command)
        .unwrap();
    let claimed = new_operation();
    fixture
        .store
        .enqueue_embedded_command(
            claimed,
            &HostCommand::Retire {
                target: command.target().clone(),
            },
        )
        .unwrap();
    fixture
        .store
        .claim_embedded_command("run", claimed)
        .unwrap()
        .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let inbox = Arc::new(
        ActorInbox::open(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(scratch.path()).unwrap(),
            "inbox.jsonl",
            "cursor.json",
        )
        .unwrap(),
    );
    let binding = embedded_harness::EmbeddedActorBinding::new(
        command.target().clone(),
        inbox,
        "test-drain".into(),
        Some(fixture.conversations[&actor].clone()),
    );
    let conversations = HashMap::from([(actor, binding)]);
    let (_, control, _channel) = harness::server::server(scratch.path().into());
    for _ in 0..2 {
        drain_embedded_browser_commands(
            &fixture.store,
            &control,
            "run",
            &fixture.nodes,
            &fixture.live,
            &fixture.projection,
            &conversations,
            &fixture.lifecycle,
        )
        .await
        .unwrap();
    }
    assert_eq!(
        fixture
            .store
            .embedded_command("run", operation_id)
            .unwrap()
            .unwrap()
            .state,
        harness::store::EmbeddedCommandState::InputAdmitted
    );
    assert_eq!(fixture.hosts[&actor].wakes.load(Ordering::Relaxed), 1);
    assert!(fixture.hosts[&actor].controls.lock().unwrap().is_empty());
}

use super::test_campaign::TestCampaign;

use exomonad_tool::{ToolArguments, ToolInvocation};

#[tokio::test]
#[ignore = "requires delegated cgroups and bubblewrap"]
async fn embedded_host_hands_out_and_executes_the_resident_command_backend() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            super::test_campaign::configure_command_path(config);
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(&authored).unwrap();
            std::fs::write(
                authored.join("AgentSpec.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/retained_shell_agent_spec.hs",
                ),
            )
            .unwrap();
            crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.spec = Some("AgentSpec.agentSpec".into());
            });
            super::test_campaign::commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    campaign.run_scenario(|campaign| Box::pin(async move {

    let owner = exomonad_node::command_resources::CommandResources::delegated(
        exomonad_node::command_resources::CommandResourcePolicy {
            general_bytes: 512 * 1024 * 1024,
            protected_bytes: 0,
            swap_max_bytes: 0,
            ..Default::default()
        },
    )
    .expect("the admitted test service delegates cgroups");
    let resources = exomonad_node::command_resources::CommandResourceClient::local(owner);
    let mut bindings = Vec::new();
    for text in ["embedded-host-command", "second-embedded-command"] {
        let policy = campaign.root_installation.policy.clone();
        let invocation = ToolInvocation {
            context: None,
            name: "bash".into(),
            arguments: ToolArguments::Structured(serde_json::json!({
                "cmd": format!("printf x >> command-reuse-executions; printf {text}"),
                "memory_mib": 256
            })),
        };
        let before = tidepool_extract_cmd::extract_spawn_count();
        let execution = tokio::spawn(async move { policy.dispatch_boxed(invocation).await });
        let request = campaign
            .next_deployment(
                "resident command backend",
                super::test_campaign::COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                |event| match event {
                    LocalResidentDeployment::CommandBackend(request) => Ok(request),
                    other => Err(other),
                },
            )
            .await;
        let actor = request.owner;
        supply_resident_command_backend(
            request,
            &campaign.config,
            &campaign.authority,
            &campaign.worktrees,
            actor,
            Some(resources.clone()),
        );

        let result = tokio::time::timeout(
            super::test_campaign::COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
            execution,
        )
        .await
        .expect("resident command settles")
        .expect("resident command task joins")
        .expect("resident command succeeds")
        .into_json()
        .expect("structured observer receives the typed workbench response");
        if !bindings.is_empty() {
            let requests = tidepool_extract_cmd::extract_spawn_count() - before;
            assert!(
                requests <= 1,
                "a new job needs only its fresh binding interface; submitted {requests} compiler requests"
            );
        }
        assert_eq!(result["status"], "committed", "{result}");
        let output = result["items"][0]["output"]
            .as_str()
            .expect("command result is presented");
        let issued_binding = result["items"][0]["installedBindings"][0]
            .as_str()
            .expect("retained command binding");
        let terminal_evidence = super::tests::dispatch_haskell_script_result(
            campaign.root_installation.policy.as_ref(),
            &format!("Cmd.await {issued_binding}"),
        )
        .await;
        let facts = &result["items"][0]["value"];
        assert_eq!(facts["state"], "Finished", "{result}; retained terminal: {terminal_evidence:?}");
        assert_eq!(facts["successful"], true, "{result}; retained terminal: {terminal_evidence:?}");
        assert_eq!(facts["outcome"], serde_json::json!({"tag": "OutcomeExited", "contents": 0}), "{result}");
        assert_eq!(facts["cleanup"], "CleanupClean", "{result}");
        assert_eq!(facts["retained_binding"], issued_binding, "{result}");
        assert_eq!(terminal_evidence.expect("retained await succeeds")["status"], "committed");
        let terminal = super::command_jobs_tests::committed(
            campaign,
            &format!("terminal <- Cmd.await {issued_binding}\nCmd.commandOutcome (Cmd.commandResult terminal) == Cmd.CommandExited 0 && Cmd.commandCleanup (Cmd.commandResult terminal) == Cmd.CommandClean"),
        )
        .await;
        assert_eq!(terminal["items"].as_array().unwrap().last().unwrap()["output"], "True", "{terminal}");
        assert!(output.contains(text), "{output}");
        bindings.push(
            result["items"][0]["installedBindings"][0]
                .as_str()
                .expect("each command installs a retained job")
                .to_owned(),
        );
    }
    assert_ne!(bindings[0], bindings[1]);
    let recovered = super::command_jobs_tests::committed(
        campaign,
        &format!(
            "retainedFirst <- Cmd.readStdout {}\nretainedSecond <- Cmd.readStdout {}\nretainedFirst == Right \"embedded-host-command\" && retainedSecond == Right \"second-embedded-command\" && {} /= {}",
            bindings[0], bindings[1], bindings[0], bindings[1],
        ),
    )
    .await;
    assert_eq!(
        recovered["items"].as_array().unwrap().last().unwrap()["output"],
        "True",
        "a later binding preserves both independently retained command outputs"
    );
    assert_eq!(
        std::fs::read(campaign.config.workspace.join("command-reuse-executions")).unwrap(),
        b"xx",
        "recovering the two jobs must not rerun either command"
    );
})).await;
}
