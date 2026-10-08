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
    entered: tokio::sync::mpsc::UnboundedSender<(usize, ResponsesRequest)>,
    rounds: Arc<std::sync::atomic::AtomicUsize>,
    release_first: Arc<tokio::sync::Semaphore>,
    release_final: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl ResponsesTransport for HeldFirstRound {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = self
            .rounds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.entered.send((round, request)).unwrap();
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
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let session_secret_file = files.path().join("session-secret");
    std::fs::write(
        &session_secret_file,
        "embedded-notification-test-secret-32-bytes",
    )
    .unwrap();
    let credential_file = files.path().join("codex-auth.json");
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
            provider: crate::exomonad::EmbeddedModelProvider::Codex,
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
                        identity,
                        campaign.actor.clone(),
                        Arc::new(
                            embedded_policy::EmbeddedPolicyInstallation::from_installation(
                                campaign.root_installation,
                            ),
                        ),
                        None,
                    )
                    .unwrap();
                let conversation = Arc::clone(&embedded.conversation);
                let binding = open_embedded_actor_binding(
                    &tidepool_atomic_write::DirectoryAnchor::open_existing(run_root).unwrap(),
                    actor,
                    harness::model::AgentPath("/root".into()),
                    Some(conversation.clone()),
                )
                .unwrap();
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
                    Some(conversation.clone()),
                )
                .unwrap();
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
    let host = hosted_test_context::HostedTestRuntime::start(&settings, &provider)
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
    let (round, _) = tokio::time::timeout(Duration::from_secs(30), rounds.recv())
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

    transport.release_first.add_permits(1);
    let (round, request) = tokio::time::timeout(Duration::from_secs(60), rounds.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(round, 2);
    assert!(
        request
            .input
            .iter()
            .any(|item| { item.0.to_string().contains("first queued row") }),
        "first input must be in the actual Engine request"
    );
    assert!(
        !request
            .input
            .iter()
            .any(|item| { item.0.to_string().contains("second queued row") }),
        "second input must remain behind the front row"
    );

    let (round, request) = tokio::time::timeout(Duration::from_secs(60), rounds.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(round, 3);
    assert!(
        request
            .input
            .iter()
            .any(|item| { item.0.to_string().contains("second queued row") }),
        "second input must progress into a later Engine request"
    );
    let operation_id =
        embedded_notification_operation_id(&binding.inbox_key, second.sequence, sender, actor);
    assert!(matches!(
        conversation
            .input_observation_by_operation(&operation_id)
            .unwrap(),
        Some(harness::embedding::InputObservation::Included(_))
    ));
    host.stop()
        .await
        .expect("production notification host acknowledges cleanup");
    let binding_alias = binding.clone();
    assert!(binding_alias.conversation().is_none());
    assert_eq!(
        binding_alias.set_conversation(Arc::clone(&conversation)),
        Err(embedded_harness::ConversationAttachError::Retired)
    );
    let observer = binding_alias
        .input_observer()
        .expect("retirement keeps read-only observer");
    assert!(matches!(
        observer
            .input_observation_by_operation(&operation_id)
            .unwrap(),
        Some(harness::embedding::InputObservation::Included(_))
    ));
    let mut notifications = JoinSet::new();
    schedule_embedded_notification_drain(actor, binding.clone(), &mut notifications);
    let (_, result) = tokio::time::timeout(Duration::from_secs(5), notifications.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    result.unwrap();
    assert!(matches!(
        binding.inbox.observe_receipt(second.sequence).unwrap(),
        ReceiptLookup::Retained(ref evidence) if evidence.phase == DeliveryPhase::Presented
    ));
}
