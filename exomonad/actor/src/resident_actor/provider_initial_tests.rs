use super::*;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

struct NativeFixture {
    forest: Arc<ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>>,
    actor: LocalActorRef,
    admission: Arc<ActorProviderAdmission>,
    _deployments: mpsc::Receiver<LocalResidentDeployment>,
    _root: Arc<tempfile::TempDir>,
    journal: Option<Arc<crate::ActorRecoveryJournal>>,
    publication: Option<crate::InitialContextPublication>,
    operation: Option<WorkbenchExecutionId>,
    _run: crate::recovery::recovery_test_fixture::RecoveryFixture,
}

impl NativeFixture {
    async fn start() -> Self {
        Self::start_with_journal(false).await
    }

    async fn start_with_journal(journal_data: bool) -> Self {
        let root = Arc::new(tempfile::tempdir().unwrap());
        let session = tidepool_runtime::session::fresh_session_id();
        let mut library =
            SessionLib::open(session, root.path(), ModuleEnv::standalone_default()).unwrap();
        let run = crate::recovery::recovery_test_fixture::RecoveryFixture::create(root.path());
        tidepool_testing::with_settlement(|settlement| run.attach_root(&mut library, settlement));
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        let (forest, deployments) = ResidentForest::new(
            ActorWorkbenchSource::new("", vec![]),
            session,
            machine,
            None,
            crate::Incarnation::FIRST,
        )
        .await
        .unwrap();
        let journal = if journal_data {
            let anchor =
                tidepool_atomic_write::DirectoryAnchor::open_existing(root.path()).unwrap();
            // The genuine native run authority and directory outlive writer drain.
            Some(
                crate::ActorRecoveryJournal::open(
                    &anchor,
                    "actors.jsonl",
                    Arc::new((run.clone(), root.clone())),
                )
                .await
                .unwrap(),
            )
        } else {
            None
        };
        let forest = Arc::new(match &journal {
            Some(journal) => forest.with_recovery_journal(journal.clone()),
            None => forest,
        });
        let placement = forest
            .environment
            .runner
            .provision_root_scope(session)
            .await
            .unwrap();
        let descriptor = ActorDescriptor::new("native provider initial", placement)
            .with_actor_path(crate::ActorPath::parse("root/provider-initial").unwrap())
            .with_persistence_policy(crate::ActorPersistencePolicy::Durable);
        let (actor, _) = crate::local_actor::spawn_local_actor_in_directory(
            None,
            ResidentKernelBehavior::with_boot(
                descriptor,
                forest.environment.clone(),
                ResidentBoot::Workbench,
                vec![],
            ),
            crate::Incarnation::FIRST,
            forest.directory.clone(),
        )
        .await
        .unwrap();
        forest
            .bind_durable_root_public_owner(actor.identity())
            .await
            .unwrap();
        let operation = journal_data.then(|| WorkbenchExecutionId::from_digest([73; 16]));
        let publication = journal_data.then(|| crate::InitialContextPublication {
            conversation: crate::ApplicationConversation::Embedded {
                run: "provider-span-data".into(),
                agent_path: "/root/provider-initial".into(),
                incarnation: "1".into(),
            },
            request: "initial-data-publication".into(),
            caller_reference: None,
            authored_reference: None,
        });
        if let Some(journal) = &journal {
            // These writer-row data exercise publication; they issue no spec or native proof.
            // The native admission above came only from the real resident owner.
            let identity = crate::InitialContextIdentity {
                operation: operation.clone().unwrap(),
                actor: actor.identity(),
                spec_install: 1,
                spec_revision: None,
                spec_entry: None,
                spec_source: "journal-row-fixture".into(),
            };
            journal
                .record_initial_context(
                    actor.identity(),
                    crate::initial_context::InitialContextEvent::Begin {
                        identity,
                        caller_seed: None,
                    },
                )
                .await
                .unwrap();
            journal
                .record_initial_context(
                    actor.identity(),
                    crate::initial_context::InitialContextEvent::Retain {
                        result: crate::RetainedContextSeed {
                            sections: vec![],
                            rendered: String::new(),
                        },
                        receipts: serde_json::json!([]),
                    },
                )
                .await
                .unwrap();
            let publication = publication.as_ref().unwrap();
            journal
                .prepare_application(
                    actor.identity(),
                    &root.path().join("application.binding"),
                    Some("journal-data"),
                    Some(publication.conversation.clone()),
                )
                .await
                .unwrap();
            journal
                .bind_application_conversation(actor.identity(), publication.conversation.clone())
                .await
                .unwrap();
        }
        // Hydrate existing retained journal data or truthfully record an absent hook.
        let observed = forest
            .prepare_initial_context(actor.identity())
            .await
            .unwrap();
        assert_eq!(
            observed.state,
            if journal_data {
                crate::InitialContextState::ResultRetained
            } else {
                crate::InitialContextState::AbsentHook
            }
        );
        let admission = forest
            .authorize_provider_attachment(actor.identity())
            .await
            .unwrap();
        Self {
            forest,
            actor,
            admission,
            _deployments: deployments,
            _root: root,
            _run: run,
            journal,
            publication,
            operation,
        }
    }

    async fn pending(&self) {
        let lifecycle = &self.admission.lifecycle;
        let mut observations = lifecycle.observations_for_test();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if observations.borrow_and_update().transactions != 0 {
                    break;
                }
                observations.changed().await.unwrap();
            }
        })
        .await
        .expect("provider command reserves before native grant");
    }
}

#[tokio::test]
async fn provider_observation_close_and_dropped_reply_settle_actual_native_grant() {
    let fixture = NativeFixture::start().await;
    let native = fixture.admission.native_readiness().unwrap();
    let held = native.begin_provider_transition().await.unwrap();
    let forest = fixture.forest.clone();
    let admission = fixture.admission.clone();
    let observer = tokio::spawn(async move {
        forest
            .initial_context_wire_observation_for_provider(&admission)
            .await
    });
    fixture.pending().await;
    fixture.admission.lifecycle.close().await.unwrap();
    assert!(fixture
        .forest
        .initial_context_wire_observation_for_provider(&fixture.admission)
        .await
        .is_err());
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    held.complete().await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.admission.lifecycle.wait_transactions(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        fixture.admission.lifecycle.observation().phase,
        crate::kernel::lifecycle::LifecyclePhase::Closing
    );
    native.check_ready_async().await.unwrap();
    assert!(fixture.admission.lifecycle.transaction().await.is_err());
    fixture.forest.shutdown().await;
}

#[tokio::test]
async fn provider_observation_refuses_fenced_native_owner_and_foreign_forest() {
    let fixture = NativeFixture::start().await;
    fixture
        .forest
        .initial_context_wire_observation_for_provider(&fixture.admission)
        .await
        .unwrap();
    let foreign = super::test_forest::TestForest::new().await;
    assert!(foreign
        .forest
        .initial_context_wire_observation_for_provider(&fixture.admission)
        .await
        .is_err());
    foreign.forest.shutdown().await;
    let native = fixture.admission.native_readiness().unwrap();
    // Abandon a real affine native transition; no readiness/fence proof is forged.
    drop(native.begin_provider_transition().await.unwrap());
    assert!(native.check_ready_async().await.is_err());
    assert!(fixture
        .forest
        .initial_context_wire_observation_for_provider(&fixture.admission)
        .await
        .is_err());
    assert_eq!(fixture.admission.lifecycle.observation().transactions, 0);
    assert_eq!(
        fixture
            .forest
            .initial_context_observation(fixture.actor.identity())
            .await
            .unwrap()
            .state,
        crate::InitialContextState::AbsentHook
    );
    fixture.forest.shutdown().await;
}

#[tokio::test]
async fn provider_publication_drop_and_close_retain_real_journal_commit() {
    let fixture = NativeFixture::start_with_journal(true).await;
    let journal = fixture.journal.as_ref().unwrap();
    let initial = journal
        .initial_context(fixture.actor.identity())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.state, crate::InitialContextState::ResultRetained);
    let resume = journal.pause_owner_for_test().await;
    let forest = fixture.forest.clone();
    let admission = fixture.admission.clone();
    let operation = fixture.operation.clone().unwrap();
    let publication = fixture.publication.clone().unwrap();
    let observer = tokio::spawn(async move {
        forest
            .acknowledge_initial_context_publication_for_provider(
                &admission,
                &operation,
                publication,
            )
            .await
    });
    fixture.pending().await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if fixture._run.store.status_async().await
                == tidepool_runtime::session::NativeStoreStatus::Pending
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual native grant precedes blocked journal work");
    assert!(
        fixture
            .admission
            .lifecycle
            .begin_fresh_binding_operation()
            .await
            .is_err(),
        "binding cannot cross an admitted publication"
    );
    // Ready check remains pending until the actual ticket's journal span settles.
    let native = fixture.admission.native_readiness().unwrap();
    let mut ready = Box::pin(native.check_ready_async());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), ready.as_mut())
            .await
            .is_err()
    );
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    fixture.admission.lifecycle.close().await.unwrap();
    assert!(fixture
        .forest
        .initial_context_wire_observation_for_provider(&fixture.admission)
        .await
        .is_err());
    resume.send(()).unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.admission.lifecycle.wait_transactions(),
    )
    .await
    .unwrap()
    .unwrap();
    ready.await.unwrap();
    let durable = journal
        .initial_context(fixture.actor.identity())
        .await
        .unwrap()
        .unwrap();
    let live = fixture
        .forest
        .initial_context_observation(fixture.actor.identity())
        .await
        .unwrap();
    assert_eq!(durable.state, crate::InitialContextState::Published);
    assert_eq!(durable.publication, fixture.publication);
    assert_eq!(live, durable);
    assert_eq!(
        fixture.admission.lifecycle.observation().phase,
        crate::kernel::lifecycle::LifecyclePhase::Closing
    );
    assert!(fixture.admission.lifecycle.transaction().await.is_err());
    fixture.forest.shutdown().await;
}
