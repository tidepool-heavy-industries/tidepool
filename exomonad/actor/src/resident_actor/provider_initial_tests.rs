use super::*;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

struct NativeFixture {
    forest: Arc<ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>>,
    actor: LocalActorRef,
    admission: Arc<ActorProviderAdmission>,
    _deployments: mpsc::Receiver<LocalResidentDeployment>,
    _root: tempfile::TempDir,
    _run: crate::recovery::recovery_test_fixture::RecoveryFixture,
}

impl NativeFixture {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
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
        let forest = Arc::new(forest);
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
        // A genuinely absent initializer still uses exact provider/native authority.
        let observed = forest
            .prepare_initial_context(actor.identity())
            .await
            .unwrap();
        assert_eq!(observed.state, crate::InitialContextState::AbsentHook);
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
        crate::LifecyclePhase::Closing
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
