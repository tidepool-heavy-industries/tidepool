//! Three actual model actors; only Responses transport is scripted.
//!
//! Provider response ownership is the deterministic barrier: both children are
//! held until the exact parent invocation is parked and its Engine yield recorded.
//! Phase rows are ordered milestones, including pending-operation boundaries.
//! Passing this case proves behavior with mocked model replies; physical timing
//! completeness belongs to the central reporter joining the retained traces.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    prepare_performance_traces, record_phase, select_hosted_script_round, HostedScriptRound,
    HostedScriptSelection, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use super::*;
use harness::model::{AgentPath, CallId, OperationId, RequestId};
use serde_json::{json, Value};
use std::{collections::VecDeque, time::Instant};

const WORKLOAD_COHORT: &str = "three-actor-capture";
const WORKLOAD_ROSTER: [&str; 9] = [
    "activation",
    "root-setup",
    "root-fork-capture",
    "async-yield",
    "child-alpha-reply",
    "child-beta-reply",
    "parent-publication-read",
    "explicit-child-cleanup",
    "host-cleanup",
];
const SETUP: &str = include_str!("fixtures/scripted_three_actor_setup.hs");
const CAPTURE: &str = include_str!("fixtures/scripted_three_actor_capture.hs");
const READ: &str = include_str!("fixtures/scripted_three_actor_read.hs");
const CLEANUP: &str = include_str!("fixtures/scripted_three_actor_cleanup.hs");
const PARENT_CALL: &str = "three-actor-capture";
const YIELD_CALL: &str = "three-actor-wait";

struct PhaseClock {
    started: Instant,
    compiler_requests: u64,
}

impl PhaseClock {
    fn begin() -> Self {
        Self {
            started: Instant::now(),
            compiler_requests: tidepool_extract_cmd::extract_spawn_count(),
        }
    }

    fn record(
        self,
        path: &Path,
        sequence: usize,
        role: &str,
        actor: &str,
        call_id: Option<&str>,
        boundary_kind: &str,
        source: &str,
        scripted_response_hold_ns: Option<u128>,
        evidence: Value,
    ) {
        record_phase(
            path,
            json!({
                "schema": 1, "phase": WORKLOAD_ROSTER[sequence], "sequence": sequence,
                "completed": true, "role": role, "actor_id": actor,
                "call_id": call_id, "boundary_kind": boundary_kind, "source": source,
                "wall_ns": self.started.elapsed().as_nanos(),
                "scripted_response_hold_ns": scripted_response_hold_ns,
                "logical_compiler_requests": tidepool_extract_cmd::extract_spawn_count() - self.compiler_requests,
                "compiler_count_scope": "global requests between ordered milestones; not per-actor attribution",
                "wall_scope": "ordered milestone window; pending calls can span later phases",
                "evidence": evidence,
            }),
        );
    }
}

fn root_identity(host: &HostedTestRuntime) -> harness::embedding::HostIdentity {
    harness::embedding::HostIdentity {
        run: runtime_namespace(&host.context.config.run_directory.path()),
        actor: AgentPath("/root".into()),
        incarnation: host.context.actor.identity().incarnation.0.to_string(),
    }
}

fn root_head(host: &HostedTestRuntime) -> RequestId {
    host.runtime
        .store()
        .embedded_round_frontier(&root_identity(host))
        .unwrap()
        .pending_head
        .expect("held root provider request has its exact pending frontier")
}

async fn next_root(
    host: &HostedTestRuntime,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut VecDeque<HostedScriptRound>,
) -> HostedScriptRound {
    host.context
        .while_root_live(
            "three-actor root provider request",
            next_hosted_script_round(requests, pending, &AgentPath("/root".into())),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
}

fn assert_pending(host: &HostedTestRuntime, operation: &OperationId) {
    let claims = host.runtime.store().claims(&operation.call).unwrap();
    let exact = claims
        .iter()
        .filter(|claim| claim.operation == *operation && claim.request == operation.request)
        .collect::<Vec<_>>();
    assert_eq!(exact.len(), 1, "one original admitted parent claim");
    assert_eq!(
        exact[0].state,
        harness::store::ClaimState::Pending,
        "held child provider replies keep the exact parent operation pending"
    );
}

fn exact_claim(
    store: &harness::store::Store,
    operation: &OperationId,
) -> Option<harness::store::Claim> {
    let mut claims = store
        .claims(&operation.call)
        .unwrap()
        .into_iter()
        .filter(|claim| claim.operation == *operation && claim.request == operation.request);
    let claim = claims.next();
    assert!(claims.next().is_none(), "one exact original yield claim");
    claim
}

async fn assert_engine_yield_owner(
    host: &HostedTestRuntime,
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    operation: &OperationId,
    state: harness::store::ClaimState,
) {
    assert_eq!(
        exact_claim(&host.runtime.store(), operation).unwrap().state,
        state
    );
    assert!(
        matches!(
            host.runtime.scheduler().output(operation).await,
            Err(harness::turn::JobError::UnknownCall)
        ),
        "Engine yield never admits a provider scheduler job"
    );
    let invocation = exomonad_tool::ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(
            embedded_harness::original_operation(&root_identity(host), operation).unwrap(),
        ),
        call_id: operation.call.0.clone(),
        namespace: None,
    };
    assert!(
        matches!(
            policy.retained_operation(invocation),
            Err(exomonad_actor::ResidentToolError::Unavailable(_))
        ),
        "Engine yield never issues a native settlement owner"
    );
}

fn assert_capture(
    round: &HostedScriptRound,
    prefix: &[harness::item::Item],
    effort: harness::model::Effort,
) {
    assert_eq!(
        round.request.model, "gpt-6-luna",
        "frozen luna alias resolves on the actual request"
    );
    let transcript = round
        .request
        .input
        .iter()
        .filter(|item| !item.is_configuration_update())
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        transcript.starts_with(prefix),
        "child retains the exact earlier provider transcript"
    );
    assert!(transcript
        .iter()
        .any(|item| item.0["call_id"] == "three-actor-setup"));
    assert!(
        transcript
            .iter()
            .all(|item| item.0["call_id"] != PARENT_CALL),
        "unfinished parent call is excluded from the BeforeCall capture"
    );
    assert_eq!(
        round
            .request
            .input
            .iter()
            .find_map(harness::item::Item::configuration_effort),
        Some(effort)
    );
    assert_eq!(round.request.pinned_effort, effort);
}

fn assert_replied(round: &HostedScriptRound, call_id: &str) {
    let receipt = round.settled_output(call_id);
    assert_eq!(receipt["status"], "replied", "{receipt}");
    assert_eq!(receipt["publication"]["status"], "published", "{receipt}");
    let items = receipt["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "one real child notebook reply");
    assert_eq!(items[0]["status"], "committed", "{receipt}");
    assert_eq!(items[0]["terminalTransfer"], "replyAccepted", "{receipt}");
    assert!(
        items[0]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation["effect"] == "reply"
                && operation["disposition"] == "committed"),
        "{receipt}"
    );
}

async fn wait_parent_parked(host: &HostedTestRuntime, operation: &OperationId) {
    let invocation = exomonad_tool::ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(
            embedded_harness::original_operation(&root_identity(host), operation).unwrap(),
        ),
        call_id: operation.call.0.clone(),
        namespace: None,
    };
    host.context
        .while_root_live(
            "three-actor parent parked on typed child settlements",
            tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                loop {
                    assert_pending(host, operation);
                    if host
                        .context
                        .actor
                        .hosted_workbench_waiting(&invocation)
                        .is_some()
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
        .expect("parent parks while both provider responses remain held");
}

#[tokio::test]
async fn engine_yield_claim_waits_and_settles_without_native_dispatch() {
    use harness::embedding::{
        AdmissionGuard, EmbeddedError, HostActor, HostControl, HostControlError, HostIdentity,
        ToolSurface,
    };
    use harness::provider::{Provider, ProviderError};
    use harness::transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError};

    struct Offline;
    impl Auth for Offline {
        fn access(&self) -> Result<(String, String), TransportError> {
            panic!("scripted yield requires no credentials")
        }
    }
    struct Guard;
    impl AdmissionGuard for Guard {}
    struct NativeSentinel;
    #[async_trait::async_trait]
    impl Provider for NativeSentinel {
        fn tools(&self) -> Vec<Value> {
            vec![]
        }
        async fn call(&self, _: &str, _: Value) -> Result<Value, ProviderError> {
            panic!("Engine yield must not reach native dispatch")
        }
    }
    struct Host {
        identity: HostIdentity,
        wakes: tokio::sync::mpsc::UnboundedSender<harness::mailbox::DurableMailboxWake>,
    }
    #[async_trait::async_trait]
    impl HostActor for Host {
        fn identity(&self) -> &HostIdentity {
            &self.identity
        }
        fn admit(&self) -> Result<Box<dyn AdmissionGuard>, EmbeddedError> {
            Ok(Box::new(Guard))
        }
        fn tool_surface(&self) -> Result<Arc<ToolSurface>, EmbeddedError> {
            Ok(Arc::new(ToolSurface::new(
                "yield-control".into(),
                vec![],
                Arc::new(NativeSentinel),
            )?))
        }
        async fn wake(&self, envelope_id: i64) -> Result<(), String> {
            self.wakes
                .send(harness::mailbox::DurableMailboxWake { envelope_id })
                .map_err(|error| error.to_string())
        }
        async fn control(&self, _: HostControl) -> Result<Value, HostControlError> {
            panic!("yield control never requests actor lifecycle changes")
        }
    }
    struct Transport(Arc<dyn harness::engine::ResponsesTransport>);
    #[async_trait::async_trait]
    impl harness::engine::ResponsesTransport for Transport {
        async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
            self.0.create(request).await
        }
    }

    for (arguments, reason) in [(json!({}), "user_input"), (json!({"until": 0}), "timeout")] {
        let store = Arc::new(harness::store::Store::memory().unwrap());
        let scheduler = Arc::new(harness::turn::JobScheduler::new(1).unwrap());
        let identity = HostIdentity {
            run: "yield-control".into(),
            actor: AgentPath("/root".into()),
            incarnation: "one".into(),
        };
        let (wakes, incoming) = tokio::sync::mpsc::unbounded_channel();
        let conversation = harness::embedding::Conversation::attach(
            store.clone(),
            Arc::new(Host {
                identity: identity.clone(),
                wakes,
            }),
            None,
        )
        .unwrap();
        let (transport, mut requests) = hosted_script_provider();
        let engine = conversation
            .engine::<Offline, _>(
                Transport(transport),
                scheduler.clone(),
                harness::engine::EngineConfig {
                    instructions: "yield claim ownership control".into(),
                    tools: vec![],
                    model: "offline".into(),
                    effort: harness::model::Effort::Low,
                    session_id: "yield-control".into(),
                    agent: identity.actor.clone(),
                },
                std::num::NonZeroU64::new(1000).unwrap(),
            )
            .unwrap();
        let (_cancel, cancellation) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            engine
                .run_embedded(None, vec![], cancellation, incoming)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            let round = requests.recv().await.unwrap();
            let operation = OperationId {
                origin: harness::model::ConversationIdentity::Embedded {
                    run: identity.run.clone(),
                    actor: identity.actor.clone(),
                    incarnation: identity.incarnation.clone(),
                },
                request: store
                    .embedded_round_frontier(&identity)
                    .unwrap()
                    .pending_head
                    .unwrap(),
                call: CallId(YIELD_CALL.into()),
            };
            round.function(YIELD_CALL, "yield", arguments);
            if reason == "user_input" {
                loop {
                    if let Some(claim) = exact_claim(&store, &operation) {
                        assert_eq!(claim.state, harness::store::ClaimState::Pending);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(
                    requests.try_recv().is_err(),
                    "indefinite yield prevents the successor request"
                );
                assert!(matches!(
                    scheduler.output(&operation).await,
                    Err(harness::turn::JobError::UnknownCall)
                ));
                conversation
                    .input("resume-yield", "/operator", "resume")
                    .await
                    .unwrap();
            }
            let round = requests.recv().await.unwrap();
            assert_eq!(
                round.settled_output(YIELD_CALL),
                json!({"reason": reason, "ready_results": []})
            );
            assert_eq!(
                exact_claim(&store, &operation).unwrap().state,
                harness::store::ClaimState::Settled
            );
            assert!(matches!(
                scheduler.output(&operation).await,
                Err(harness::turn::JobError::UnknownCall)
            ));
            round.finish();
            task.await.unwrap().unwrap();
        })
        .await
        .expect("exact yield claim and owned wake settle without native dispatch");
    }
}

#[tokio::test]
#[ignore = "requires isolated owned matched resident compiler, prepared bundle inputs and retained traces"]
async fn production_harness_three_actor_capture_phases() {
    let (host_trace, compiler_trace, phase_trace, lifecycle) = prepare_performance_traces();
    let deployment: BTreeMap<_, _> = [
        "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT",
        "TIDEPOOL_PREPARED_ROOT_ENTRY",
        "TIDEPOOL_PREPARED_BUILTIN_ENTRIES",
        "TIDEPOOL_COMPILER_DEPLOYMENT",
        "TIDEPOOL_COMPILER_MODULES",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH",
    ]
    .into_iter()
    .map(|key| {
        (
            key,
            std::env::var(key).unwrap_or_else(|_| panic!("required frozen resident input {key}")),
        )
    })
    .collect();
    assert!(std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_some());
    record_phase(
        &phase_trace,
        json!({
            "schema": 1, "phase": "environment", "workload_cohort": WORKLOAD_COHORT,
            "campaign_trace_profile": super::test_campaign::campaign_trace_profile(),
            "workload_roster": WORKLOAD_ROSTER, "prepared_root_entry_supplied": true,
            "host_trace": host_trace, "compiler_trace": compiler_trace, "phase_trace": phase_trace,
            "owned_compiler_lifecycle": lifecycle, "deployment": deployment,
            "provider_mode": "scripted Responses transport; all runtime/compiler/workspace owners real",
        }),
    );
    let activation = PhaseClock::begin();
    let files = tempfile::TempDir::new().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let authored_settings = settings.clone();
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, |config| {
        config.model = "gpt-6.1-sol".into();
        let authored = config.workspace.join(".exomonad");
        crate::exomonad::write_fixture_project_config(&authored, "gpt-6.1-sol", |project| {
            project.launch.embedded = Some(authored_settings);
            project.models.insert("luna".into(), "gpt-6-luna".into());
            project.defaults.effort = crate::exomonad::ExomonadEffort::Low;
        });
        commit_workspace(&config.workspace);
    })
    .await
    .expect("prepare and start through the production frozen workspace owner");
    let cleanup_clock = Arc::new(Mutex::new(None));
    let cleanup_after = Arc::clone(&cleanup_clock);
    let scenario_phases = phase_trace.clone();
    host.run_scenario(|host| Box::pin(async move {
        let root = host.context.actor.identity();
        let root_name = root.to_string();
        let frozen = host.context.config.workspace_inputs.as_ref().unwrap();
        let coverage = frozen.prepared_toolset_coverage().unwrap();
        assert_eq!(coverage.len(), 1, "one prepared root toolset");
        let installation = host.context.observer.installation(root).await;
        assert_eq!(installation.acquisition.as_ref().and_then(exomonad_actor::ToolsetAcquisition::selection),
            Some(coverage[0].program.clone()), "root installs its exact prepared program");
        host.http_input("Exercise the real three-actor capture workload.").await.unwrap();
        let mut pending = VecDeque::new();
        let mut round = next_root(host, &mut requests, &mut pending).await;
        assert_eq!(round.request.model, "gpt-6.1-sol");
        let root_session = round.request.session_id.clone();
        activation.record(&scenario_phases, 0, "root", &root_name, None, "lifecycle", "",
            None,
            json!({"prepared_root_original": true, "http_input_admitted": true,
                "workspace_preparation_ns": host.preparation_elapsed_ns(),
                "host_start_readiness_ns": host.startup_readiness_elapsed_ns()}));

        let phase = PhaseClock::begin();
        let scripted_response_hold_ns = round.scripted_response_hold_ns();
        round.call("three-actor-setup", SETUP);
        round = next_root(host, &mut requests, &mut pending).await;
        round.assert_value("three-actor-setup", "True");
        phase.record(&scenario_phases, 1, "root", &root_name, Some("three-actor-setup"), "native-tool", SETUP,
            Some(scripted_response_hold_ns), json!({"retained_value": 41, "cell_role": "first"}));
        let prefix = round.request.input.iter().filter(|item| !item.is_configuration_update())
            .cloned().collect::<Vec<_>>();
        let effort = round.request.input.iter().rev().find_map(harness::item::Item::configuration_effort).unwrap();
        let parent = OperationId { origin: round.origin(), request: root_head(host), call: CallId(PARENT_CALL.into()) };
        let phase = PhaseClock::begin();
        let scripted_response_hold_ns = round.scripted_response_hold_ns();
        round.async_call(PARENT_CALL, CAPTURE);
        let successor = next_root(host, &mut requests, &mut pending).await;
        assert!(!successor.request.input.iter().any(|item| item.0["call_id"] == PARENT_CALL
            && item.0["type"] == "custom_tool_call_output"), "held child replies prevent parent completion");
        let mut children: Vec<(String, ActorRef, HostedScriptRound)> = Vec::new();
        while children.len() < 2 {
            let child_round = host.context.while_root_live(
                "three-actor held worker provider requests",
                select_hosted_script_round(
                    &mut requests, &mut pending,
                    HostedScriptSelection::OtherThan(&AgentPath("/root".into())),
                ),
            ).await.unwrap_or_else(|error| panic!("{error}"));
            assert_ne!(child_round.origin().actor(), &AgentPath("/root".into()));
            assert_capture(&child_round, &prefix, effort);
            let graph = host.context.forest.inspect_host_graph();
            let child_identity = child_round.host_identity();
            let node = graph.iter().find(|node| host.context.binding(node.actor)
                .and_then(|binding| binding.conversation())
                .is_some_and(|conversation| conversation.identity() == &child_identity)).unwrap();
            assert!(matches!(node.label.as_str(), "three-actor-alpha" | "three-actor-beta"));
            assert_eq!(node.creator, Some(root));
            assert_eq!(node.supervisor_parent, Some(root));
            assert_eq!(node.context_parent, Some(root));
            assert!(node.bound_worktree.is_some());
            assert!(node.model_actor);
            assert!(children.iter().all(|(label, _, _)| label != &node.label));
            children.push((node.label.clone(), node.actor, child_round));
        }
        children.sort_by(|a, b| a.0.cmp(&b.0));
        let graph = host.context.forest.inspect_host_graph();
        assert_eq!(graph.len(), 3, "one root and exactly two model workers");
        assert!(graph.iter().all(|node| node.model_actor));
        assert_ne!(graph.iter().find(|node| node.actor == children[0].1).unwrap().bound_worktree,
            graph.iter().find(|node| node.actor == children[1].1).unwrap().bound_worktree, "distinct actual fork worktrees");
        wait_parent_parked(host, &parent).await;
        let parent_invocation = exomonad_tool::ToolInvocationContext {
            origin: exomonad_tool::ToolInvocationOrigin::Model(
                embedded_harness::original_operation(&root_identity(host), &parent).unwrap(),
            ),
            call_id: parent.call.0.clone(), namespace: None,
        };
        assert!(installation.policy.retained_operation(parent_invocation).is_ok(),
            "the real parent supplies the positive native settlement control");
        phase.record(&scenario_phases, 2, "root", &root_name, Some(PARENT_CALL), "native-tool", CAPTURE,
            Some(scripted_response_hold_ns),
            json!({"milestone": "both captured workers admitted while parent parked", "operation": parent,
                "model_actors": 3, "held_worker_responses": 2}));

        let phase = PhaseClock::begin();
        let yield_head = root_head(host);
        let yield_operation = OperationId {
            origin: successor.origin(), request: yield_head.clone(), call: CallId(YIELD_CALL.into()),
        };
        let scripted_response_hold_ns = successor.scripted_response_hold_ns();
        successor.function(YIELD_CALL, "yield", json!({}));
        host.context.while_root_live("three-actor persisted Engine yield",
            tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                loop {
                    assert_pending(host, &parent);
                    if host.runtime.store().replay_turns(&yield_head).unwrap().iter().any(|turn|
                        turn.request == yield_head && turn.model_response.items.iter().any(|item|
                            item.0["type"] == "function_call" && item.0["name"] == "yield" && item.0["call_id"] == YIELD_CALL))
                        && exact_claim(&host.runtime.store(), &yield_operation).is_some() { break; }
                    tokio::task::yield_now().await;
                }
            })).await.unwrap_or_else(|error| panic!("{error}"))
            .expect("yield recorded while both scripted worker responses remain held");
        // Engine claims its intrinsic for recovery, without dispatching it to
        // the provider or issuing a native workbench settlement owner.
        assert_engine_yield_owner(host, installation.policy.as_ref(), &yield_operation,
            harness::store::ClaimState::Pending).await;
        phase.record(&scenario_phases, 3, "root", &root_name, Some(YIELD_CALL), "engine-builtin", "yield {}",
            Some(scripted_response_hold_ns),
            json!({"milestone": "yield persisted with exact parent Pending and both child responses held", "parent_operation": parent,
                "yield_operation": yield_operation, "yield_claim_pending": true, "native_yield_owner": false}));

        let child_actors = children.iter().map(|(_, actor, _)| *actor).collect::<Vec<_>>();
        for (index, (label, actor, child_round)) in children.into_iter().enumerate() {
            let phase = PhaseClock::begin();
            let call = format!("three-actor-child-{}", if index == 0 { "alpha" } else { "beta" });
            let origin = child_round.origin();
            let scripted_response_hold_ns = child_round.scripted_response_hold_ns();
            child_round.call(&call, "respond capturedGetter");
            let completed = host.context.while_root_live("three-actor native child reply",
                next_hosted_script_round(&mut requests, &mut pending, origin.actor()))
                .await.unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(completed.request.model, "gpt-6-luna");
            assert_replied(&completed, &call);
            if index == 0 { assert_pending(host, &parent); }
            phase.record(&scenario_phases, 4 + index, &label, &actor.to_string(), Some(&call), "native-tool", "respond capturedGetter",
                Some(scripted_response_hold_ns),
                json!({"typed_reply_accepted": true, "expected_value": 42}));
            completed.finish();
        }
        let phase = PhaseClock::begin();
        round = next_root(host, &mut requests, &mut pending).await;
        let scripted_response_hold_ns = round.scripted_response_hold_ns();
        assert_eq!(round.request.session_id, root_session);
        assert_eq!(round.request.model, "gpt-6.1-sol");
        round.assert_value(PARENT_CALL, "True");
        let yielded = round.settled_output(YIELD_CALL);
        assert_eq!(yielded["reason"], "tool_result", "{yielded}");
        assert!(yielded["ready_results"].as_array().unwrap().iter().any(|result| result["call"] == PARENT_CALL), "{yielded}");
        assert_engine_yield_owner(host, installation.policy.as_ref(), &yield_operation,
            harness::store::ClaimState::Settled).await;
        round.call("three-actor-read", READ);
        round = next_root(host, &mut requests, &mut pending).await;
        round.assert_value("three-actor-read", "True");
        phase.record(&scenario_phases, 6, "root", &root_name, Some("three-actor-read"), "native-tool", READ,
            Some(scripted_response_hold_ns),
            json!({"parent_published": true, "retained_action_reused": true, "yield_parent_result_observed": true,
                "cell_role": "retained_action_reuse"}));

        let phase = PhaseClock::begin();
        let scripted_response_hold_ns = round.scripted_response_hold_ns();
        round.call("three-actor-cleanup", CLEANUP);
        round = next_root(host, &mut requests, &mut pending).await;
        round.assert_value("three-actor-cleanup", "True");
        for actor in child_actors {
            let installed = host.context.observer.installation(actor).await;
            assert!(installed.checkpoint);
            assert_eq!(installed.context_parent, Some(root));
            let terminal = installed.actor.terminal();
            tokio::time::timeout(Duration::from_secs(30), terminal.wait()).await.expect("explicit stop retires exact child");
            let cleanup = terminal.cleanup().expect("retirement retains resource cleanup evidence");
            assert_eq!(cleanup.actor(), actor);
            assert!(cleanup.is_confirmed(), "{cleanup:?}");
        }
        phase.record(&scenario_phases, 7, "root", &root_name, Some("three-actor-cleanup"), "native-tool", CLEANUP,
            Some(scripted_response_hold_ns),
            json!({"checkpoint_release_idempotent": true, "child_cleanup_confirmed": 2}));
        *cleanup_clock.lock() = Some(PhaseClock::begin());
        round.finish();
    })).await;
    cleanup_after
        .lock()
        .take()
        .expect("scenario reached its cleanup boundary")
        .record(
            &phase_trace,
            8,
            "host",
            "host",
            None,
            "lifecycle",
            "",
            None,
            json!({"production_host_shutdown_acknowledged": true}),
        );
}

#[cfg(test)]
mod roster_tests {
    use super::{WORKLOAD_COHORT, WORKLOAD_ROSTER};

    #[test]
    fn performance_fixture_roster_matches_report_contract() {
        let manifest: Vec<String> =
            serde_json::from_str(include_str!("fixtures/three_actor_workload_roster.json"))
                .expect("shared performance roster is valid JSON");
        assert_eq!(WORKLOAD_COHORT, "three-actor-capture");
        assert_eq!(manifest, WORKLOAD_ROSTER);
        assert_eq!(manifest.first().map(String::as_str), Some("activation"));
        assert_eq!(manifest.last().map(String::as_str), Some("host-cleanup"));
        assert!(manifest
            .iter()
            .any(|phase| phase == "explicit-child-cleanup"));
    }
}
