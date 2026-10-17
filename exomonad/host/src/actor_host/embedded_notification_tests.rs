use super::delivery::deliver_embedded_notifications;
use super::test_campaign::TestCampaign;
use super::*;
use async_trait::async_trait;
use exomonad_node::{DeliveryPhase, ReceiptLookup};
use harness::{
    engine::ResponsesTransport,
    item::Item,
    transport::{ResponsesRequest, ResponsesTurn, TransportError},
};
use serde_json::json;

#[derive(Clone)]
struct HeldFirstRound {
    entered:
        tokio::sync::mpsc::UnboundedSender<(usize, harness::model::RequestId, ResponsesRequest)>,
    rounds: Arc<std::sync::atomic::AtomicUsize>,
    release_first: Arc<tokio::sync::Semaphore>,
    release_final: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl ResponsesTransport for HeldFirstRound {
    fn response_contract(
        &self,
    ) -> Result<
        (
            harness::transport::ResponsesRoute,
            harness::transport::ResponsesProtocol,
        ),
        harness::transport::TransportError,
    > {
        Ok((
            harness::transport::ResponsesRoute::ChatGptPlan,
            harness::transport::ResponsesProtocol::Standard,
        ))
    }

    async fn create(
        &self,
        request: harness::transport::PreparedProviderRequest,
    ) -> Result<ResponsesTurn, TransportError> {
        let issued_request = request.request_id().clone();
        let request = request.request().clone();
        let round = self
            .rounds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.entered.send((round, issued_request, request)).unwrap();
        match round {
            1 => {
                self.release_first
                    .acquire()
                    .await
                    .expect("first model round release")
                    .forget();
                Ok(tool_turn("notification-test-1"))
            }
            2 => Ok(tool_turn("notification-test-2")),
            3 => {
                self.release_final
                    .acquire()
                    .await
                    .expect("final model round release")
                    .forget();
                Ok(ResponsesTurn {
                    response_id: "notification-test-final".into(),
                    items: vec![Item(json!({
                        "type":"message", "role":"assistant", "phase":"final_answer",
                        "content":[{"type":"output_text","text":"done"}]
                    }))],
                    usage: Default::default(),
                })
            }
            other => panic!("unexpected model request round {other}"),
        }
    }
}

fn tool_turn(response_id: &str) -> ResponsesTurn {
    ResponsesTurn {
        response_id: response_id.into(),
        items: vec![Item(json!({
            "type":"custom_tool_call", "call_id":response_id,
            "name":"haskell", "input":"40 + 2 :: Int"
        }))],
        usage: Default::default(),
    }
}

fn embedded_settings() -> (tempfile::TempDir, crate::exomonad::EmbeddedLaunchConfig) {
    let files = tempfile::tempdir().unwrap();
    let assets = super::test_campaign::hosted_browser_assets();
    let session_secret_file = files.path().join("session-secret");
    std::fs::write(
        &session_secret_file,
        "embedded-notification-test-secret-32-bytes",
    )
    .unwrap();
    let credential_file = files.path().join("chatgpt-auth.json");
    std::fs::write(&credential_file, "{}").unwrap();
    (
        files,
        crate::exomonad::EmbeddedLaunchConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
            public_origin: None,
            asset_root: assets,
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
            session_secret_file: Some(session_secret_file),
            provider: exomonad_actor::Backend::OpenAIPlan,
            inference_limits: Default::default(),
            credential_file,
            context_capacity_tokens: 200_000,
            concurrent_jobs: 1,
        },
    )
}

#[tokio::test]
async fn embedded_notification_handoff_retries_by_operation_and_waits_for_store_inclusion() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let actor = campaign.actor.identity();
                let run_root = campaign.session_root.path();
                let runtime = embedded_harness::EmbeddedHarnessRuntime::open(run_root, 1).unwrap();
                let identity = harness::embedding::HostIdentity {
                    run: runtime_namespace(run_root),
                    actor: harness::model::AgentPath("/root".into()),
                    incarnation: actor.incarnation.0.to_string(),
                };
                let embedded = runtime
                    .attach(
                        provider_attachment::ProviderAttachment::admit(
                            campaign.forest.clone(),
                            actor,
                        )
                        .await
                        .unwrap(),
                        identity,
                        campaign.actor.clone(),
                        Arc::new(
                            embedded_policy::EmbeddedPolicyInstallation::from_installation(
                                &campaign.root_installation,
                            ),
                        ),
                        None,
                    )
                    .await
                    .unwrap();
                let conversation = Arc::clone(&embedded.conversation);
                let binding = open_embedded_actor_binding(
                    &tidepool_atomic_write::DirectoryAnchor::open_existing(run_root).unwrap(),
                    actor,
                    harness::model::AgentPath("/root".into()),
                )
                .unwrap();
                binding.set_conversation(conversation.clone()).unwrap();
                let context = DeliveryProvenance::Notification {
                    sender: actor,
                    target: actor,
                };
                let row = binding
                    .inbox
                    .publish_tracked(
                        DurableActorEvent::Text("replay-safe embedded notification".into()),
                        context.clone(),
                    )
                    .unwrap();

                deliver_embedded_notifications(actor, binding.clone())
                    .await
                    .unwrap();
                let first = binding.inbox.observe_receipt(row.sequence).unwrap();
                assert!(matches!(
                    first,
                    ReceiptLookup::Retained(ref evidence)
                        if evidence.context == context && evidence.phase == DeliveryPhase::Submitted
                ));
                let unread = runtime.store().unread("/root").unwrap();
                assert_eq!(unread.len(), 1);
                let item = runtime
                    .store()
                    .get_item(&unread[0].item_hash)
                    .unwrap()
                    .unwrap();
                assert_eq!(item.0["content"], "replay-safe embedded notification");
                assert!(matches!(
                    conversation.input_observation(unread[0].id).unwrap(),
                    harness::embedding::InputObservation::Admitted
                ));

                drop(binding);
                let reloaded = open_embedded_actor_binding(
                    &tidepool_atomic_write::DirectoryAnchor::open_existing(run_root).unwrap(),
                    actor,
                    harness::model::AgentPath("/root".into()),
                )
                .unwrap();
                reloaded.set_conversation(conversation.clone()).unwrap();
                deliver_embedded_notifications(actor, reloaded.clone())
                    .await
                    .unwrap();
                let after_retry = runtime.store().unread("/root").unwrap();
                assert_eq!(
                    after_retry.len(),
                    1,
                    "stable operation id deduplicates the retry"
                );
                assert_eq!(after_retry[0].item_hash, unread[0].item_hash);
                assert!(matches!(
                    reloaded.inbox.observe_receipt(row.sequence).unwrap(),
                    ReceiptLookup::Retained(ref evidence)
                        if evidence.context == context && evidence.phase == DeliveryPhase::Submitted
                ));
            })
        })
        .await;
}

#[tokio::test]
async fn production_engine_advances_queued_notifications_and_reconciles_inclusion() {
    let (_files, settings) = embedded_settings();
    let (entered, mut rounds) = tokio::sync::mpsc::unbounded_channel();
    let transport = HeldFirstRound {
        entered,
        rounds: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        release_first: Arc::new(tokio::sync::Semaphore::new(0)),
        release_final: Arc::new(tokio::sync::Semaphore::new(0)),
    };
    let provider: Arc<dyn ResponsesTransport> = Arc::new(transport.clone());
    let mut host = hosted_test_context::HostedTestRuntime::start(&settings, &provider)
        .await
        .expect("production notification host starts");
    host.input("Begin the round that receives queued notifications.")
        .await
        .unwrap();
    let actor = host.context.actor.identity();
    let binding = host
        .context
        .binding(actor)
        .expect("production root binding attached");
    let conversation = binding.conversation().unwrap();
    assert_eq!(binding.set_conversation(Arc::clone(&conversation)), Ok(()));
    let mut wrong_identity = conversation.identity().clone();
    wrong_identity.actor = harness::model::AgentPath("/different".into());
    let wrong_binding = embedded_harness::EmbeddedActorBinding::new(
        wrong_identity,
        Arc::clone(&binding.inbox),
        binding.inbox_key.clone(),
        None,
    );
    assert_eq!(
        wrong_binding.set_conversation(Arc::clone(&conversation)),
        Err(embedded_harness::ConversationAttachError::IdentityMismatch)
    );
    let (round, _, _) = tokio::time::timeout(Duration::from_secs(30), rounds.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(round, 1);
    let sender = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(90));
    let context = DeliveryProvenance::Notification {
        sender,
        target: actor,
    };
    let first = binding
        .inbox
        .publish_tracked(
            DurableActorEvent::Text("first queued row".into()),
            context.clone(),
        )
        .unwrap();
    let second = binding
        .inbox
        .publish_tracked(DurableActorEvent::Text("second queued row".into()), context)
        .unwrap();

    deliver_embedded_notifications(actor, binding.clone())
        .await
        .unwrap();
    deliver_embedded_notifications(actor, binding.clone())
        .await
        .unwrap();
    assert!(matches!(
        binding.inbox.observe_receipt(first.sequence).unwrap(),
        ReceiptLookup::Retained(ref evidence) if evidence.phase == DeliveryPhase::Submitted
    ));
    assert!(matches!(
        binding.inbox.observe_receipt(second.sequence).unwrap(),
        ReceiptLookup::Retained(ref evidence) if evidence.phase == DeliveryPhase::Accepted
    ));

    let notification_positions = |items: &[Item], text: &str| {
        items
            .iter()
            .enumerate()
            .filter_map(|(position, item)| {
                (item.0["type"] == "message"
                    && item.0["role"] == "user"
                    && item.0["content"] == text)
                    .then_some(position)
            })
            .collect::<Vec<_>>()
    };
    transport.release_first.add_permits(1);
    let (round, second_request, request) =
        tokio::time::timeout(Duration::from_secs(60), rounds.recv())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(round, 2);
    let first_positions = notification_positions(&request.input, "first queued row");
    let second_positions = notification_positions(&request.input, "second queued row");
    assert_eq!(
        first_positions.len(),
        1,
        "first input must occur exactly once"
    );
    assert!(
        second_positions.len() <= 1,
        "second input must not be duplicated"
    );
    // Reconciliation can advance the next row between the Engine's inbox
    // admissions while one provider request is being prepared.
    let second_in_round_two = if let Some(position) = second_positions.first() {
        assert!(
            first_positions[0] < *position,
            "queued input order must be FIFO"
        );
        true
    } else {
        false
    };

    let (round, third_request, request) =
        tokio::time::timeout(Duration::from_secs(60), rounds.recv())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(round, 3);
    let first_positions = notification_positions(&request.input, "first queued row");
    let second_positions = notification_positions(&request.input, "second queued row");
    assert_eq!(
        first_positions.len(),
        1,
        "first input remains exactly once in history"
    );
    assert_eq!(
        second_positions.len(),
        1,
        "second input must progress exactly once"
    );
    assert!(
        first_positions[0] < second_positions[0],
        "queued input order must remain FIFO"
    );
    let store = host.runtime.store();
    let assert_inclusion = |operation: &str, issued: &harness::model::RequestId, text: &str| {
        let Some(harness::embedding::InputObservation::Included(included)) = conversation
            .input_observation_by_operation(operation)
            .unwrap()
        else {
            panic!("notification operation must have retained inclusion");
        };
        assert_eq!(
            notification_positions(&store.items(&included).unwrap(), text).len(),
            1,
            "issuing attachment head must contain the notification exactly once"
        );
        let mut cursor = issued.clone();
        let mut visited = std::collections::HashSet::new();
        while cursor != included {
            assert!(
                visited.insert(cursor.clone()),
                "request lineage must not cycle"
            );
            cursor = store
                .request(&cursor)
                .unwrap()
                .expect("issued request lineage must remain retained")
                .parent
                .expect("notification attachment head must precede its provider request");
        }
        included
    };
    let first_operation_id =
        embedded_notification_operation_id(&binding.inbox_key, first.sequence, sender, actor);
    assert_inclusion(&first_operation_id, &second_request, "first queued row");
    let second_issued_request = if second_in_round_two {
        second_request
    } else {
        third_request
    };
    let operation_id =
        embedded_notification_operation_id(&binding.inbox_key, second.sequence, sender, actor);
    let second_included_head =
        assert_inclusion(&operation_id, &second_issued_request, "second queued row");
    host.shutdown_execution()
        .await
        .expect("production notification executor acknowledges shutdown");
    let binding_alias = binding.clone();
    assert!(binding_alias.conversation().is_none());
    assert_eq!(
        binding_alias.set_conversation(Arc::clone(&conversation)),
        Err(embedded_harness::ConversationAttachError::Retired)
    );
    let observer = binding_alias
        .input_observer()
        .expect("retirement keeps read-only observer");
    // Conversation retains execution/boot authority; InputObserver retains only read facts.
    drop(conversation);
    host.stop(&crate::actor_host::hosted_test_context::ScenarioOutcome::Passed)
        .await
        .expect("production notification host acknowledges physical cleanup");
    assert!(matches!(
        observer
            .input_observation_by_operation(&operation_id)
            .unwrap(),
        Some(harness::embedding::InputObservation::Included(ref included)) if included == &second_included_head
    ));
    tokio::time::timeout(
        Duration::from_secs(5),
        deliver_embedded_notifications(actor, binding.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    for sequence in [first.sequence, second.sequence] {
        assert!(matches!(
            binding.inbox.observe_receipt(sequence).unwrap(),
            ReceiptLookup::Retained(ref evidence) if evidence.phase == DeliveryPhase::Presented
        ));
    }
}
