//! One history through provider issuance, ordinary workspace edits and coherent reload.
use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    select_hosted_script_round, HostedScriptRound, HostedScriptSelection,
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

struct UnexpectedJudgment(AtomicUsize);
impl exomonad_actor::JevBackend for UnexpectedJudgment {
    fn ask(
        &self,
        _packet: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(exomonad_actor::JevCallFailure::JevTransport(
                "this history requires no semantic judgment".into(),
            ))
        })
    }
}

/// Hold only the real source owner's blocking preparation boundary. Other
/// interpreters and actors retain their production scheduling paths.
#[derive(Default)]
struct ReloadGate {
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    release: std::sync::Condvar,
}
impl ReloadGate {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
    fn wait_if_armed(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.release.wait(released).unwrap();
            }
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_all();
    }
}
struct ReleaseReloadGate(Arc<ReloadGate>);
impl Drop for ReleaseReloadGate {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct GatedSourceLayers {
    owner: exomonad_actor::ActorSourceLayerResolver,
    gate: Arc<ReloadGate>,
    observe_gate: Option<Arc<ReloadGate>>,
}
impl exomonad_actor::ActorSourceLayers for GatedSourceLayers {
    fn freeze_toolset_layer(
        &self,
        actor: tidepool_repr::PrincipalId,
    ) -> Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.owner.freeze_toolset_layer(actor)
    }
    fn toolset_layer_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.owner.toolset_layer_from(source)
    }
    fn fresh_toolset_layer_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.owner.fresh_toolset_layer_from(source)
    }
    fn validate_source_authority(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> Result<(), String> {
        self.owner.validate_source_authority(source)
    }
    fn freeze_checkpoint_layer(
        &self,
        actor: tidepool_repr::PrincipalId,
    ) -> Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.owner.freeze_checkpoint_layer(actor)
    }
    fn admit_checkpoint_layer(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
        actor: tidepool_repr::PrincipalId,
        branch: &str,
    ) -> Result<Vec<std::path::PathBuf>, String> {
        self.owner.admit_checkpoint_layer(source, actor, branch)
    }
    fn admit_retained_layer(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> Result<Vec<std::path::PathBuf>, String> {
        self.owner.admit_retained_layer(source)
    }
    fn bind_checkpoint_for(
        &self,
        actor: tidepool_repr::PrincipalId,
        branch: &str,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> Result<(), String> {
        self.owner.bind_checkpoint_for(actor, branch, source)
    }
    fn retain_helpers(&self, actor: tidepool_repr::PrincipalId) -> Result<String, String> {
        self.owner.retain_helpers(actor)
    }
    fn layer_include_for(&self, branch: &str) -> Result<Vec<std::path::PathBuf>, String> {
        self.owner.layer_include_for(branch)
    }
    fn bind_for(&self, actor: tidepool_repr::PrincipalId, branch: &str) {
        self.owner.bind_for(actor, branch)
    }
    fn restore_source(
        &self,
        reference: &tidepool_runtime::session::GenerationArtifactReference,
    ) -> Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.owner.restore_source(reference)
    }
    fn restore_source_with_acquisition(
        &self,
        reference: &tidepool_runtime::session::GenerationArtifactReference,
        acquisition: &exomonad_actor::ToolsetAcquisition,
    ) -> Result<exomonad_actor::CheckpointSourceLayer, exomonad_actor::SourceInspectionError> {
        self.owner
            .restore_source_with_acquisition(reference, acquisition)
    }
    fn read_from(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
        module: &str,
    ) -> Result<(String, String), exomonad_actor::SourceInspectionError> {
        self.owner.read_from(source, module)
    }
    fn revision(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> Result<tidepool_bridge_effects::SrRevision, exomonad_actor::SourceInspectionError> {
        self.owner.revision(source)
    }
    fn observe(
        &self,
        source: &exomonad_actor::CheckpointSourceLayer,
    ) -> Result<tidepool_bridge_effects::SrStatus, exomonad_actor::SourceInspectionError> {
        if let Some(gate) = &self.observe_gate {
            gate.wait_if_armed();
        }
        self.owner.observe(source)
    }
    fn stage_reload(
        self: Arc<Self>,
        actor: tidepool_repr::PrincipalId,
        source: &exomonad_actor::CheckpointSourceLayer,
        checks: &[String],
    ) -> Result<Box<dyn exomonad_actor::StagedActorSourceReload>, exomonad_actor::SourceLayerReload>
    {
        self.gate.wait_if_armed();
        self.owner.clone().stage_reload(actor, source, checks)
    }
}

/// Compare the authored page through its exact receipt and retained Store owner.
fn assert_true_page(host: &HostedTestRuntime, result: &Value) {
    let actor = host.context.actor.identity();
    assert_eq!(
        super::test_campaign::retained_committed_display_text(
            &host.runtime.store(),
            &harness::store::actor_output::ActorOutputOrigin {
                run: host.runtime.run_identity().to_owned(),
                native_actor: actor.id.0,
                incarnation: actor.incarnation.0,
            },
            result,
        ),
        "True",
    );
}

fn assert_reload_disposition(
    result: &Value,
    expected: tidepool_runtime::session::WorkbenchOperationDisposition,
) {
    let operations: Vec<_> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["operations"].as_array().into_iter().flatten())
        .filter(|operation| operation["effect"] == "reloadSource")
        .collect();
    assert_eq!(
        operations.len(),
        1,
        "one authored reload must retain its own receipt: {result}"
    );
    assert_eq!(
        operations[0]["disposition"],
        serde_json::to_value(expected).unwrap(),
        "{result}"
    );
}

async fn selected_root_generation(
    host: &HostedTestRuntime,
) -> tidepool_runtime::session::ActiveNotebookGeneration {
    let actor = host.context.actor.identity();
    let placement = host
        .context
        .forest
        .root_recovery_placement(actor)
        .await
        .expect("live actor incarnation");
    tidepool_runtime::session::RunRecoveryStore::read_notebook_generation_at(
        host.context.recovery_store.path(),
        tidepool_runtime::session::RecoveryDomainId::Root,
        placement.owner(),
    )
    .expect("read exact active notebook generation")
    .expect("published actor generation has a durable selected head")
}

fn assert_reload_tool(result: &Value, outcome: &str) {
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{result}");
    assert_eq!(result["status"], "committed", "{result}");
    assert_eq!(items[0]["value"]["Right"]["tag"], outcome, "{result}");
}

fn configure_workshop(
    config: &mut super::ActorHostConfig,
    judgments: Arc<UnexpectedJudgment>,
    spec: Option<&str>,
) {
    configure_workshop_inventory(config, judgments, spec, 0);
}

fn configure_workshop_inventory(
    config: &mut super::ActorHostConfig,
    judgments: Arc<UnexpectedJudgment>,
    spec: Option<&str>,
    inventory_modules: usize,
) {
    config.jev = Some(judgments.clone());
    super::startup_persona_acceptance_tests::configure_persona(
        config,
        "AgentSpec.agentSpec",
        |workspace, project| {
            let authored = workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::write(
                authored.join("Project/Work.hs"),
                include_str!("fixtures/coherent_reload_work_a.hs"),
            )
            .unwrap();
            project.haskell.modules.push("Project.Work".into());
            let inputs = workspace.join("reload-inputs");
            std::fs::create_dir_all(&inputs).unwrap();
            if let Some(spec) = spec {
                std::fs::write(authored.join("AgentSpec.hs"), spec).unwrap();
            }
            if inventory_modules != 0 {
                let inventory = authored.join("Inventory");
                std::fs::create_dir_all(&inventory).unwrap();
                for ordinal in 0..inventory_modules {
                    let name = format!("M{ordinal:04}");
                    std::fs::write(
                        inventory.join(format!("{name}.hs")),
                        format!("module Inventory.{name} where\nvalue :: Int\nvalue = {ordinal}\n"),
                    )
                    .unwrap();
                }
            }
            for (name, body) in [
                ("spec-b.txt", include_str!("fixtures/sync_reload_spec_b.hs")),
                (
                    "work-a.txt",
                    include_str!("fixtures/coherent_reload_work_a.hs"),
                ),
                (
                    "work-b.txt",
                    include_str!("fixtures/coherent_reload_work_b.hs"),
                ),
                (
                    "work-invalid.txt",
                    include_str!("fixtures/coherent_reload_work_invalid.hs"),
                ),
                (
                    "prompt-b.txt",
                    include_str!("fixtures/coherent_reload_prompt_b.md"),
                ),
            ] {
                std::fs::write(inputs.join(name), body).unwrap();
            }
        },
    );
    let selected = config.workspace_inputs.as_ref().unwrap();
    assert!(selected
        .modules
        .iter()
        .any(|module| module == "Project.Work"));
    let work = selected
        .captured_source_roots()
        .iter()
        .map(|root| root.join("Project/Work.hs"))
        .find(|path| path.is_file())
        .expect("the selected immutable graph contains Work");
    assert_eq!(
        std::fs::read_to_string(work).unwrap(),
        include_str!("fixtures/coherent_reload_work_a.hs")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn coherent_reload_replaces_the_notebook_and_prompts_without_rewriting_issued_work() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let judgments = Arc::new(UnexpectedJudgment(AtomicUsize::new(0)));
    let observe_gate = Arc::new(ReloadGate::default());
    let transform = {
        let observe_gate = observe_gate.clone();
        Arc::new(move |owner| {
            Arc::new(GatedSourceLayers {
                owner,
                gate: Default::default(),
                observe_gate: Some(observe_gate.clone()),
            }) as exomonad_actor::ActorSourceLayerResolver
        })
    };
    let host = HostedTestRuntime::start_configured_with_source_layers(
        &settings,
        &provider,
        |config| {
            configure_workshop(config, judgments.clone(), None);
        },
        transform,
    )
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            let _release_observation_on_failure = ReleaseReloadGate(observe_gate.clone());
            let root = harness::model::AgentPath("/root".into());
            let mut pending = VecDeque::new();
            let retained = host
                .context
                .forest
                .initial_context_observation(host.context.actor.identity())
                .await
                .unwrap();
            assert_eq!(
                retained.state,
                exomonad_actor::InitialContextState::Published
            );
            let context = retained.result.as_ref().unwrap().rendered.clone();
            let installation = host
                .context
                .observer
                .installation(host.context.actor.identity())
                .await;
            let old_request = installation
                .policy
                .snapshot_for_request()
                .expect("issued generation A");
            host.input("Keep our workshop conversation through this edit.")
                .await
                .unwrap();
            let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let session = round.request.session_id.clone();
            let instructions_a = round.request.instructions.clone();
            assert_eq!(
                old_request.request_instructions(),
                Some(instructions_a.as_str())
            );
            let operation = round.operation("bind-a");
            round.sync_call_named(
                "bind-a",
                "haskell",
                include_str!("fixtures/coherent_reload_bind_a.hs"),
            );
            round = host
                .while_operation_succeeds(
                    &operation,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            assert_true_page(host, &round.settled_output("bind-a"));
            let baseline = super::tests::dispatch_status(old_request.as_ref(), "revisions").await;
            assert_eq!(baseline["status"], "committed");
            let observed_a = installation.runtime_observation.snapshot().source_drift.layer.current().expect("explicit baseline capture").sample.clone();
            observe_gate.arm();
            let refresh_policy = old_request.clone();
            let refresh = tokio::spawn(async move { super::tests::dispatch_status(refresh_policy.as_ref(), "revisions").await });
            host.context.while_root_live("source observation captured generation A", observe_gate.entered.notified()).await.unwrap();
            let operation = round.operation("publish-b");
            round.sync_call_named(
                "publish-b",
                "haskell",
                include_str!("fixtures/coherent_reload_publish_b.hs"),
            );
            round = host
                .while_operation_succeeds(
                    &operation,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            assert_true_page(host, &round.settled_output("publish-b"));
            assert_reload_disposition(
                &round.settled_output("publish-b"),
                tidepool_runtime::session::WorkbenchOperationDisposition::Committed,
            );
            let durable_b = selected_root_generation(host).await;
            observe_gate.release();
            let refresh_result = host.context.while_root_live("superseded source observation rejoins", refresh).await.unwrap().unwrap();
            assert_eq!(refresh_result["status"], "committed", "inspection reports unavailability as data");
            assert!(matches!(installation.runtime_observation.snapshot().source_drift.layer,
                exomonad_actor::SourceObservation::Unavailable { last_known: Some(previous), .. } if previous.sample == observed_a),
                "capture A cannot publish current after generation B");
            let current_observation = super::tests::dispatch_status(old_request.as_ref(), "revisions").await;
            assert_eq!(current_observation["status"], "committed");
            let observed_b = installation.runtime_observation.snapshot().source_drift.layer.current().unwrap().sample.clone();
            assert_eq!(observed_b.issued_notebook_generation, old_request.request_generation().map(|id| id.get()));
            assert_ne!(observed_b.issued_notebook_generation, observed_b.active_notebook_generation);
            assert_eq!(observed_b.issued_identity.as_ref(), Some(&observed_a.active_identity));
            assert_ne!(observed_b.issued_identity.as_ref(), Some(&observed_b.active_identity));
            assert_eq!(observed_b.active_identity, observed_b.disk_identity);
            assert_eq!(
                old_request.request_instructions(),
                Some(instructions_a.as_str()),
                "issued A prompt bytes remain immutable after B"
            );
            let instructions_b = serde_json::to_value(&round.request).unwrap()["instructions"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_ne!(instructions_b, instructions_a);
            assert!(instructions_b.contains(include_str!("fixtures/coherent_reload_prompt_b.md")));
            let late = super::tests::dispatch_haskell_script(
                old_request.as_ref(),
                include_str!("fixtures/coherent_reload_late_a.hs"),
            )
            .await;
            assert_eq!(
                late["status"], "committed",
                "issued A must remain usable: {late}"
            );
            assert_true_page(host, &late);
            assert_eq!(
                selected_root_generation(host).await,
                durable_b,
                "late A private publication leaves B's exact durable selected generation intact"
            );
            let current = host
                .context
                .observer
                .installation(host.context.actor.identity())
                .await;
            let missing =
                super::tests::dispatch_haskell_script(current.policy.as_ref(), "display aOnly")
                    .await;
            assert_eq!(
                missing["status"], "rejected",
                "fresh B must not inherit A names: {missing}"
            );
            for (id, source) in [
                ("use-b", include_str!("fixtures/coherent_reload_use_b.hs")),
                ("noop", include_str!("fixtures/coherent_reload_noop.hs")),
                ("reject", include_str!("fixtures/coherent_reload_reject.hs")),
                (
                    "after-rejection",
                    include_str!("fixtures/coherent_reload_after_rejection.hs"),
                ),
            ] {
                assert_eq!(round.request.session_id, session);
                assert_eq!(round.request.instructions, instructions_b);
                super::startup_context_acceptance_tests::assert_request_user_texts(
                    host,
                    host.context.actor.identity(),
                    &round.request,
                    &[
                        context.as_str(),
                        "Keep our workshop conversation through this edit.",
                    ],
                )
                .await;
                let operation = round.operation(id);
                round.sync_call_named(id, "haskell", source);
                round = host
                    .while_operation_succeeds(
                        &operation,
                        next_hosted_script_round(&mut requests, &mut pending, &root),
                    )
                    .await
                    .unwrap();
                assert_true_page(host, &round.settled_output(id));
                match id {
                    "noop" => assert_reload_disposition(
                        &round.settled_output(id),
                        tidepool_runtime::session::WorkbenchOperationDisposition::Read,
                    ),
                    "reject" => assert_reload_disposition(
                        &round.settled_output(id),
                        tidepool_runtime::session::WorkbenchOperationDisposition::Rejected,
                    ),
                    _ => {}
                }
                if matches!(id, "noop" | "reject") {
                    let authored = super::tests::dispatch_structured_tool(
                        current.policy.as_ref(),
                        "reload",
                        serde_json::json!({"also_check": []}),
                    )
                    .await;
                    assert_reload_tool(
                        &authored,
                        if id == "noop" {
                            "ReloadUnchanged"
                        } else {
                            "ReloadRejected"
                        },
                    );
                }
            }
            assert_eq!(round.request.instructions, instructions_b);
            assert_eq!(
                host.context
                    .forest
                    .initial_context_observation(host.context.actor.identity())
                    .await
                    .unwrap(),
                retained
            );
            assert_eq!(
                judgments.0.load(Ordering::SeqCst),
                0,
                "startup and reload must not invoke Jev"
            );
            drop(old_request);
            round.finish();
        })
    })
    .await;
}

fn authored_tool_names(request: &harness::transport::ResponsesRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .filter(|name| !matches!(*name, "status" | "yield"))
        .map(str::to_owned)
        .collect()
}

async fn authored_cell(
    host: &HostedTestRuntime,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut VecDeque<HostedScriptRound>,
    root: &harness::model::AgentPath,
    round: HostedScriptRound,
    id: &str,
    source: &str,
) -> HostedScriptRound {
    let operation = round.operation(id);
    round.sync_call_named(id, "haskell", source);
    let next = host
        .while_operation_succeeds(
            &operation,
            next_hosted_script_round(requests, pending, root),
        )
        .await
        .unwrap();
    assert_true_page(host, &next.settled_output(id));
    next
}

async fn authored_reload(
    host: &HostedTestRuntime,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut VecDeque<HostedScriptRound>,
    root: &harness::model::AgentPath,
    round: HostedScriptRound,
    id: &str,
    name: &str,
    arguments: Value,
) -> HostedScriptRound {
    let operation = round.operation(id);
    round.function(id, name, arguments);
    host.while_operation_succeeds(
        &operation,
        next_hosted_script_round(requests, pending, root),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn standard_reload_retains_complete_large_source_receipts_after_publication() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let judgments = Arc::new(UnexpectedJudgment(AtomicUsize::new(0)));
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        // Source inventory is issued by the production capture owner. These
        // unchanged modules enlarge the receipt without adding compilation work.
        configure_workshop_inventory(
            config,
            judgments.clone(),
            Some(include_str!("fixtures/sync_reload_spec_a.hs")),
            450,
        );
    })
    .await
    .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        let root = harness::model::AgentPath("/root".into());
        let mut pending = VecDeque::new();
        host.input("Publish this source edit once and retain its complete receipt.").await.unwrap();
        let round = host.while_host_running(
            next_hosted_script_round(&mut requests, &mut pending, &root),
        ).await.unwrap();
        let operation = round.operation("large-reload");
        let context = super::embedded_harness::embedded_invocation_context(
            &round.host_identity(), &operation,
        ).unwrap();
        let tools_a = authored_tool_names(&round.request);
        let instructions_a = round.request.instructions.clone();
        let generation_a = selected_root_generation(host).await;
        let installation_a = host.context.observer.installation(host.context.actor.identity()).await;
        let authored = host.context.config.workspace.join(".exomonad");
        std::fs::write(authored.join("Project/Work.hs"),
            include_str!("fixtures/coherent_reload_work_b.hs")).unwrap();
        std::fs::write(authored.join("prompts/agent.md"),
            include_str!("fixtures/coherent_reload_prompt_b.md")).unwrap();
        round.function("large-reload", "apply_draft", serde_json::json!({}));
        let round = host.while_operation_succeeds(&operation,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        let model = round.settled_output("large-reload");
        assert_eq!(model["status"], "committed");
        assert_reload_disposition(&model,
            tidepool_runtime::session::WorkbenchOperationDisposition::Committed);
        let shown = model["items"][0]["output"].as_str().unwrap();
        assert!(shown.starts_with("Published source "), "{shown}");
        assert!(shown.len() < 1024);
        assert!(model["items"][0].get("value").is_none());
        assert_eq!(model["items"][0]["valueOmitted"], serde_json::json!({
            "reason":"modelBudget", "limitBytes":28 * 1024,
            "nativeReceipt":{"availability":"nativeOnly","operation":context},
        }));
        assert!(serde_json::to_vec(&model).unwrap().len() < 28 * 1024,
            "the provider sees the compact presentation and omission receipt, not the full inventories");
        let native = installation_a.policy.retained_operation(context).unwrap().terminal();
        let exomonad_actor::HostedOperationTerminal::Settled(
            exomonad_actor::WorkbenchCancellationOutcome::PublicationSettled {
                reply: Ok(receipt), ..
            },
        ) = native else { panic!("the issued native reload must retain its successful terminal receipt") };
        let value = receipt.items[0].value.as_ref().expect("full native semantic value");
        let bytes = serde_json::to_vec(value).unwrap().len();
        assert!(bytes > 100_000, "receipt must exceed the former display observation budget: {bytes}");
        let contents = &value["Right"]["contents"];
        assert_eq!(value["Right"]["tag"], "ReloadPublished");
        for revision in [0, 1] {
            let modules = contents[revision]["revisionModules"].as_array().unwrap();
            assert!(modules.len() >= 450);
            assert!(modules.iter().any(|module| module["sourceModuleName"] == "Inventory.M0449"));
        }
        assert_ne!(contents[0]["revisionIdentity"], contents[1]["revisionIdentity"]);
        assert!(contents[2].as_array().unwrap().contains(&serde_json::json!("Project.Work")));
        assert_ne!(selected_root_generation(host).await, generation_a);
        assert_eq!(authored_tool_names(&round.request), tools_a);
        assert_ne!(round.request.instructions, instructions_a);
        assert!(round.request.instructions.contains(include_str!("fixtures/coherent_reload_prompt_b.md")));
        assert_eq!(judgments.0.load(Ordering::SeqCst), 0);
        round.finish();
    })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authored_sync_reload_publishes_an_opted_in_surface_and_waits_for_settlement() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 2);
    let (provider, mut requests) = hosted_script_provider();
    let judgments = Arc::new(UnexpectedJudgment(AtomicUsize::new(0)));
    let gate = Arc::new(ReloadGate::default());
    let transform: super::hosted_test_context::HostSourceLayersTransform = {
        let gate = gate.clone();
        Arc::new(move |owner| {
            Arc::new(GatedSourceLayers {
                owner,
                gate: gate.clone(),
                observe_gate: None,
            })
        })
    };
    let host = HostedTestRuntime::start_configured_with_source_layers(
        &settings,
        &provider,
        |config| {
            configure_workshop(
                config,
                judgments.clone(),
                Some(include_str!("fixtures/sync_reload_spec_a.hs")),
            )
        },
        transform,
    )
    .await
    .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        let _release_on_failure = ReleaseReloadGate(gate.clone());
        let root = harness::model::AgentPath("/root".into());
        let mut pending = VecDeque::new();
        host.input("Keep working while our tool interface evolves.").await.unwrap();
        let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        let root_identity = round.host_identity();
        let tools_a = authored_tool_names(&round.request);
        assert_eq!(tools_a, ["haskell", "apply_draft"]);
        let instructions_a = round.request.instructions.clone();
        let installation = host.context.observer.installation(host.context.actor.identity()).await;
        let issued_a = installation.policy.snapshot_for_request().unwrap();
        let durable_a = selected_root_generation(host).await;
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "bind-a",
            include_str!("fixtures/coherent_reload_bind_a.hs")).await;
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "invalid-draft",
            include_str!("fixtures/sync_reload_invalid_draft.hs")).await;
        round = authored_reload(host, &mut requests, &mut pending, &root, round,
            "invalid-refusal", "apply_draft", serde_json::json!({})).await;
        assert_reload_tool(&round.settled_output("invalid-refusal"), "ReloadRejected");
        assert_eq!(selected_root_generation(host).await, durable_a);
        assert_eq!(authored_tool_names(&round.request), tools_a);
        assert_eq!(round.request.instructions, instructions_a);
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "draft-b",
            include_str!("fixtures/sync_reload_publish_b_draft.hs")).await;
        round = authored_reload(host, &mut requests, &mut pending, &root, round,
            "surface-refusal", "apply_draft", serde_json::json!({})).await;
        let refused = round.settled_output("surface-refusal");
        assert_reload_tool(&refused, "ReloadRefused");
        let refusal = &refused["items"][0]["value"]["Right"]["contents"][2];
        assert_eq!(refusal["refusalCategory"], "SurfaceIncompatible");
        let diff = &refusal["refusalSurface"];
        let added = diff["surfaceAdded"].as_array().unwrap();
        assert!(added.contains(&serde_json::json!("web_search")), "{diff}");
        assert!(added.contains(&serde_json::json!("activate_draft")), "{diff}");
        assert!(diff["surfaceRemoved"].as_array().unwrap().contains(&serde_json::json!("apply_draft")), "{diff}");
        assert_eq!(selected_root_generation(host).await, durable_a);
        assert_eq!(authored_tool_names(&round.request), tools_a);
        assert_eq!(round.request.instructions, instructions_a);
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "progress-child",
            include_str!("fixtures/sync_reload_child_progress.hs")).await;
        let child = host.while_host_running(select_hosted_script_round(
            &mut requests, &mut pending, HostedScriptSelection::OtherThan(&root))).await.unwrap();
        let child_path = child.host_identity().actor;
        let reload_operation = round.operation("accept-surface");
        let mut settlements = host.runtime.scheduler().operation_settlements();
        gate.arm();
        round.function("accept-surface", "apply_draft", serde_json::json!({"accept_cache_invalidation":true}));
        host.while_operation_succeeds(&reload_operation, gate.entered.notified()).await.unwrap();
        let input = host.input("This input arrived while synchronous reload was preparing.").await.unwrap();
        let before = host.runtime.store().embedded_round_frontier(&root_identity).unwrap();
        assert_eq!(before.pending_head.as_ref(), Some(&reload_operation.request));
        let child_operation = child.operation("child-progress");
        child.sync_call_named("child-progress", "haskell", "respond (73 :: Int)");
        let child_done = host.while_operation_succeeds(&child_operation,
            next_hosted_script_round(&mut requests, &mut pending, &child_path)).await.unwrap();
        child_done.finish();
        let store = host.runtime.store();
        assert!(store.claims_for_operation(&reload_operation).unwrap().iter()
            .any(|claim| claim.operation == reload_operation && claim.state == harness::store::ClaimState::Pending));
        assert!(store.replay_output_operation(&reload_operation).unwrap().is_none());
        assert_eq!(store.embedded_round_frontier(&root_identity).unwrap(), before,
            "input admission and another actor's real progress cannot advance the held synchronous root request");
        let admitted_input = store.envelope(input.envelope_id).unwrap().unwrap();
        assert!(admitted_input.delivered_request.is_none());
        assert!(pending.iter().all(|queued| queued.host_identity().actor != root));
        gate.release();
        host.while_host_running(async {
            loop {
                if settlements.recv().await.unwrap() == reload_operation { break; }
            }
        }).await.unwrap();
        round = host.while_operation_succeeds(&reload_operation,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        assert_reload_tool(&round.settled_output("accept-surface"), "ReloadPublished");
        assert_eq!(authored_tool_names(&round.request), ["web_search", "activate_draft", "haskell"]);
        assert!(round.request.instructions.contains(include_str!("fixtures/coherent_reload_prompt_b.md")));
        assert_ne!(round.request.instructions, instructions_a);
        assert_eq!(issued_a.request_instructions(), Some(instructions_a.as_str()));
        let durable_b = selected_root_generation(host).await;
        assert_ne!(durable_b, durable_a);
        let late = super::tests::dispatch_haskell_script(issued_a.as_ref(),
            include_str!("fixtures/coherent_reload_late_a.hs")).await;
        assert_true_page(host, &late);
        assert_eq!(selected_root_generation(host).await, durable_b);
        let current = host.context.observer.installation(host.context.actor.identity()).await;
        let missing = super::tests::dispatch_haskell_script(current.policy.as_ref(), "display aOnly").await;
        assert_eq!(missing["status"], "rejected");
        let search_operation = round.operation("new-tool");
        round.function("new-tool", "web_search", serde_json::json!({"query":"needle"}));
        round = host.while_operation_succeeds(&search_operation,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        assert_eq!(round.settled_output("new-tool")["items"][0]["value"], "B:needle:42");
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "use-b",
            include_str!("fixtures/coherent_reload_use_b.hs")).await;
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "memory-only",
            include_str!("fixtures/sync_reload_noop_draft.hs")).await;
        round = authored_reload(host, &mut requests, &mut pending, &root, round,
            "no-change", "activate_draft", serde_json::json!({"accept_cache_invalidation":false})).await;
        assert_reload_tool(&round.settled_output("no-change"), "ReloadUnchanged");
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "names-survive-noop",
            include_str!("fixtures/sync_reload_after_noop.hs")).await;
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "invalid-b-draft",
            include_str!("fixtures/sync_reload_invalid_b_draft.hs")).await;
        round = authored_reload(host, &mut requests, &mut pending, &root, round,
            "invalid-b-refusal", "activate_draft", serde_json::json!({})).await;
        assert_reload_tool(&round.settled_output("invalid-b-refusal"), "ReloadRejected");
        round = authored_cell(host, &mut requests, &mut pending, &root, round, "after-refusal",
            include_str!("fixtures/coherent_reload_after_rejection.hs")).await;
        assert_eq!(selected_root_generation(host).await, durable_b);
        assert_eq!(judgments.0.load(Ordering::SeqCst), 0);
        drop(issued_a);
        round.finish();
    })).await;
}
