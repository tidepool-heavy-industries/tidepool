use super::*;
use crate::{ActorId, ActorPlacement, ActorSourceImports, ActorWorkbenchSource};
use tidepool_codegen::scope::ScopeId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    registry::SlotKind, CheckoutError, ModuleEnv, ResidentError, SessionError, SessionLib,
};

type Registry = ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>;
type Workbench = ResidentActorWorkbench<frunk::HNil, tidepool_mcp::CapturedOutput>;

fn context(session: SessionId, scope: ScopeId) -> ActorSessionContext {
    ActorSessionContext {
        actor: ActorRef::first(ActorId(41)),
        placement: ActorPlacement {
            session,
            resource_scope: RealmId::ROOT,
            lexical_scope: scope,
        },
        effect_policy: EffectRunPolicy::HandleOrSuspend,
        live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        source_imports: ActorSourceImports::default(),
        haskell_effects_alias: "[]".into(),
        current_model: None,
        source_layer: Vec::new().into(),
    }
}

fn fixture(
    preparation: crate::CompilerPreparationClient,
) -> (
    Arc<Registry>,
    Workbench,
    SessionId,
    ScopeId,
    tempfile::TempDir,
) {
    let machines = Arc::new(Registry::new());
    let session_id = SessionId(11);
    let root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default()).unwrap();
    let mut session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let retired = session.mint_isolated_scope();
    session.retire_scope(retired);
    machines.insert_idle(session_id, Box::new(session));
    let workbench = Workbench::new(
        Arc::clone(&machines),
        ActorWorkbenchSource::new("", Vec::new()).bind_preparation(preparation),
        None,
    );
    (machines, workbench, session_id, retired, root)
}

async fn unknown_session(request: InspectionRequest) {
    crate::resident_workbench::with_test_compiler_owner(|client| {
        unknown_session_with_owner(client, request)
    })
    .await;
}

async fn unknown_session_with_owner(
    preparation: crate::CompilerPreparationClient,
    request: InspectionRequest,
) {
    let (machines, workbench, other, _, _root) = fixture(preparation.clone());
    let missing = SessionId(12);
    let before = machines.peek(other, ResidentSession::run_context).unwrap();
    let error = inspect(&workbench, context(missing, ScopeId::ROOT), request)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ResidentActorWorkbenchError::Checkout(CheckoutError::Unknown(id)) if id == missing
        ),
        "unexpected inspection refusal: {error:?}"
    );
    assert_eq!(machines.kind(missing), None);
    assert_eq!(machines.kind(other), Some(SlotKind::Idle));
    assert_eq!(
        machines.peek(other, ResidentSession::run_context),
        Some(before)
    );
}

async fn retired_scope(request: InspectionRequest) {
    crate::resident_workbench::with_test_compiler_owner(|client| {
        retired_scope_with_owner(client, request)
    })
    .await;
}

async fn retired_scope_with_owner(
    preparation: crate::CompilerPreparationClient,
    request: InspectionRequest,
) {
    let (machines, workbench, session, retired, _root) = fixture(preparation.clone());
    let before = machines
        .peek(session, ResidentSession::run_context)
        .unwrap();
    let error = inspect(&workbench, context(session, retired), request)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ResidentActorWorkbenchError::Resident(ResidentError::Session(SessionError::DeadScope(scope)))
                if scope == retired
        ),
        "unexpected inspection refusal: {error:?}"
    );
    assert_eq!(machines.kind(session), Some(SlotKind::Idle));
    assert_eq!(
        machines.peek(session, ResidentSession::run_context),
        Some(before)
    );

    // A rejected inspection must return checkout custody so a later valid read runs.
    let result = inspect(
        &workbench,
        context(session, ScopeId::ROOT),
        InspectionRequest::Live,
    )
    .await
    .unwrap();
    assert!(matches!(result, InspectionResult::Live(bindings) if bindings.is_empty()));
    assert_eq!(machines.kind(session), Some(SlotKind::Idle));
}

#[tokio::test]
async fn compiler_inspection_requires_cleanup_owner_before_checkout() {
    crate::resident_workbench::with_test_compiler_owner(|preparation| async move {
        // Source preparation lifetime is explicit, while this new task has no
        // inherited task-local compiler owner for the inspection being refused.
        tokio::spawn(
            async move { compiler_inspection_without_task_local_owner(preparation).await },
        )
        .await
        .unwrap();
    })
    .await;
}

async fn compiler_inspection_without_task_local_owner(
    preparation: crate::CompilerPreparationClient,
) {
    assert!(crate::capabilities::CompilerOwnerScope::current().is_err());
    let compiler_spawns = tidepool_extract_cmd::extract_spawn_count();
    for request in [InspectionRequest::Recovery, InspectionRequest::Bindings] {
        let (machines, workbench, session, _, _root) = fixture(preparation.clone());
        let before = machines
            .peek(session, ResidentSession::run_context)
            .unwrap();
        let error = inspect(&workbench, context(session, ScopeId::ROOT), request)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                ResidentActorWorkbenchError::CompilerCleanupOwnerUnavailable
            ),
            "unexpected inspection refusal: {error:?}"
        );
        assert_eq!(machines.kind(session), Some(SlotKind::Idle));
        assert_eq!(
            machines.peek(session, ResidentSession::run_context),
            Some(before)
        );
    }
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        compiler_spawns,
        "missing task-local inspection authority must refuse before compiler work"
    );
}

#[tokio::test]
async fn recovery_refuses_unknown_session_without_using_another_machine() {
    unknown_session(InspectionRequest::Recovery).await;
}

#[tokio::test]
async fn bindings_refuses_unknown_session_without_using_another_machine() {
    unknown_session(InspectionRequest::Bindings).await;
}

#[tokio::test]
async fn live_refuses_unknown_session_without_using_another_machine() {
    unknown_session(InspectionRequest::Live).await;
}

#[tokio::test]
async fn recovery_refuses_retired_scope_and_returns_checkout() {
    retired_scope(InspectionRequest::Recovery).await;
}

#[tokio::test]
async fn bindings_refuses_retired_scope_and_returns_checkout() {
    retired_scope(InspectionRequest::Bindings).await;
}

#[tokio::test]
async fn live_refuses_retired_scope_and_returns_checkout() {
    retired_scope(InspectionRequest::Live).await;
}
