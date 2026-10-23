//! Real compiler-issued result roots reused across lifecycle histories.
//! Each test process compiles its immutable programs once; each reply executes a
//! fresh native request wrapper and captures its assembled ResponseResult.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use tidepool_bridge::{FromHaskell, HaskellValue};
use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
use tidepool_effect::{EffectError, EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{Generation, SessionId};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates,
    turn::{run_turn, BoundBinder, CompiledTurn, TurnRequest, TurnResult},
    ModuleEnv, OutputSink, PersistentSession, ResidentError, ResidentHole, ResidentOutcome,
    ResidentSession, RuntimeResultPublication, SessionLib,
};
use tidepool_runtime::RuntimeError;
use tidepool_testing::effect_surface::TestEffectSurface;

use super::{RequestId, RequestRegistry, RequestReplyClaim, WatchNotification};
use crate::{
    owned_result::{OwnedResultSnapshot, RequestResultDestination},
    request_effect::RepliesReq,
    ActorRef,
};

#[derive(Clone)]
struct NoOutput;
impl OutputSink for NoOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

type Machine = ResidentSession<frunk::HNil, NoOutput>;
const FIXTURE_SESSION: SessionId = SessionId(0xCA71);

struct Fixture {
    _compile_root: tempfile::TempDir,
    _machine_root: tempfile::TempDir,
    resident: Machine,
    context: crate::ActorSessionContext,
    receiver: CompiledTurn,
    bound: Vec<BoundBinder>,
    include: Vec<PathBuf>,
    producer: Arc<CompiledTurn>,
}

fn suspended(outcome: ResidentOutcome) -> (ResidentHole, HaskellValue) {
    match outcome {
        ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        other => panic!("result fixture must suspend: {other:?}"),
    }
}

impl Fixture {
    fn new() -> Self {
        Self::with_source(include_str!("fixtures/result-request.hs"))
    }

    fn with_source(source: &str) -> Self {
        Self::with_source_for_actor(source, ActorRef::first(crate::ActorId(1)))
    }

    fn with_source_for_actor(source: &str, actor: ActorRef) -> Self {
        tidepool_testing::eval_harness::require_extract();
        let surface = TestEffectSurface::minimal(&[
            tidepool_mcp::agent_tools_decl(),
            tidepool_mcp::agent_session_decl(),
            tidepool_mcp::actor_decl(),
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
            tidepool_mcp::fs_read_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::notifications_decl(),
            tidepool_mcp::console_decl(),
            tidepool_mcp::sleep_decl(),
        ])
        .expect("real request fixture effect vocabulary");
        let root = tempfile::tempdir().unwrap();
        let mut include = surface.include_paths().to_vec();
        include.push(PathBuf::from(
            std::env::var_os("TIDEPOOL_HASKELL_ACTORS_DIR")
                .expect("declared actor sources must be available"),
        ));
        let mut preamble = surface.preamble().to_owned();
        for import in [
            "Tidepool.Agent.Reply (Replies)",
            "qualified Tidepool.Agent.Reply.Internal as Replies",
            "Tidepool.Agent.Ref.Internal (AgentProtocol(..))",
            "qualified Tidepool.Agent.Ref.Internal as Ref",
            "qualified Tidepool.Actors.Internal.Agent as Agents",
            "qualified Tidepool.Effects.Core as Core",
        ] {
            preamble = insert_preamble_imports(&preamble, import);
        }
        let library = SessionLib::open(
            FIXTURE_SESSION,
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(include.clone());
        let view = PersistentSession::new(Some(library), tidepool_runtime::DEFAULT_NURSERY_SIZE)
            .compile_view_in(ScopeId::ROOT)
            .unwrap();
        let imports = view.turn_imports(&Default::default());
        let templates = resident_workbench_templates(&preamble, "'[Replies]", &imports);
        let include = view.include_paths(&include);
        let paths = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let compile = |source: &str, gen| {
            tidepool_testing::with_settlement(|settlement| {
                run_turn(
                    TurnRequest {
                        exact_context: None,
                        session_id: None,
                        turn_text: source,
                        templates: &templates,
                        include: &paths,
                        session_root: view.session_root(),
                        inject_modules: &[],
                        gen,
                        verdict: None,
                        retained_imports: &[],
                    },
                    settlement,
                )
            })
            .expect("compile immutable result fixture through original native issuer")
        };
        let TurnResult::Bind {
            compiled: receiver,
            bound,
            ..
        } = compile(include_str!("fixtures/result-receiver.hs"), 1)
        else {
            panic!("receiver fixture must supply native bindings")
        };
        assert_eq!(bound.len(), 3);
        let producer = match compile(source, 2) {
            TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => compiled,
            TurnResult::Decl(_) => panic!("request fixture must execute"),
        };
        let (machine_root, resident, context) = Self::machine(&receiver, &bound, &include, actor);
        Self {
            _compile_root: root,
            _machine_root: machine_root,
            resident,
            context,
            receiver,
            bound,
            include,
            producer: Arc::new(producer),
        }
    }

    fn machine(
        receiver: &CompiledTurn,
        bound: &[BoundBinder],
        include: &[PathBuf],
        actor: ActorRef,
    ) -> (tempfile::TempDir, Machine, crate::ActorSessionContext) {
        let root = tempfile::tempdir().unwrap();
        let library = SessionLib::open(
            FIXTURE_SESSION,
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(include.to_vec());
        let mut resident = Machine::unbootstrapped(
            frunk::HNil,
            NoOutput,
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        let scope = resident.mint_isolated_scope();
        let mut context = crate::ActorDescriptor::new(
            "request-result-fixture",
            crate::ActorPlacement {
                session: FIXTURE_SESSION,
                resource_scope: RealmId::ROOT,
                lexical_scope: scope,
            },
        )
        .admit_independent_world()
        .session_context(actor);
        context.effect_policy = EffectRunPolicy::SuspendAll;
        context.live_payload = LivePayloadPolicy::HASKELL_EFFECT_VALUE;
        context
            .source_dependency()
            .adopt_final_scope(&mut resident, scope)
            .unwrap();
        resident
            .set_actor_execution(
                context.run_context(),
                context.effect_policy,
                context.live_payload,
            )
            .unwrap();
        let source = context.source_dependency().source_lease().unwrap();
        let installed = resident
            .with_source_compiler_transaction(
                source.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |resident, settlement| {
                    resident.run_projected_bind_with_sites(
                        "result-fixture-native-bindings",
                        receiver.code(),
                        bound,
                        Generation(1),
                        settlement,
                    )
                },
            )
            .unwrap();
        assert!(
            matches!(
                installed.close(),
                tidepool_runtime::CompilerTransactionClose::Clean
                    | tidepool_runtime::CompilerTransactionClose::NotStarted
            ),
            "initial binding close: {:?}",
            installed.close()
        );
        assert!(matches!(
            installed.into_outcome().action.unwrap(),
            ResidentOutcome::BindingsCommitted { .. }
        ));
        (root, resident, context)
    }

    fn run<T>(
        &mut self,
        action: impl FnOnce(
            &mut Machine,
            &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
        ) -> T,
    ) -> T {
        let source = self.context.source_dependency().source_lease().unwrap();
        let settled = self
            .resident
            .with_source_compiler_transaction(
                source.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                action,
            )
            .unwrap();
        assert!(
            matches!(
                settled.close(),
                tidepool_runtime::CompilerTransactionClose::Clean
                    | tidepool_runtime::CompilerTransactionClose::NotStarted
            ),
            "request fixture close: {:?}",
            settled.close()
        );
        settled.into_outcome().action
    }

    fn reset(&mut self) {
        assert_eq!(
            self.resident.outstanding_custody(),
            0,
            "a completed generated history must release all native custody"
        );
        let (root, resident, context) = Self::machine(
            &self.receiver,
            &self.bound,
            &self.include,
            self.context.actor,
        );
        self.resident = resident;
        self.context = context;
        self._machine_root = root;
    }

    fn submission(&mut self, request: RequestId) -> (ResidentHole, HaskellValue, u64) {
        let producer = self.producer.clone();
        let (reservation, _) = suspended(
            self.run(|resident, settlement| {
                resident.run_with_sites("result-fixture-request", producer.code(), settlement)
            })
            .unwrap(),
        );
        let (submission, payload) = suspended(
            self.run(|resident, settlement| {
                resident.resume(
                    reservation,
                    Ok::<i64, ()>(i64::try_from(request.0).unwrap()),
                    settlement,
                )
            })
            .unwrap(),
        );
        let RepliesReq::SubmitRequestWith(_, site, _, _, _) =
            RepliesReq::from_value(&payload, self.resident.data_con_table()).unwrap()
        else {
            panic!("reservation must reach exact typed submission")
        };
        (submission, payload, u64::try_from(site).unwrap())
    }

    fn destination(&mut self, owner: ActorRef, request: RequestId) -> RequestResultDestination {
        let (hole, _, site) = self.submission(request);
        let witness = self
            .resident
            .request_result_type_witness(site, &hole)
            .unwrap();
        self.abort_submission(&hole, "fixture destination admitted");
        RequestResultDestination::new(
            owner,
            FIXTURE_SESSION,
            witness,
            self.resident.lease_bindings(&[]),
        )
    }

    fn publish(&mut self, request: RequestId) -> RuntimeResultPublication {
        let (submission, _, site) = self.submission(request);
        let payload = self
            .resident
            .live_payload_handle_owned_by(submission.cont_id(), RealmId::ROOT)
            .unwrap()
            .expect("submission owns original RunRequest");
        self.abort_submission(&submission, "fixture payload transferred");
        let scope = self.context.placement.lexical_scope;
        let expected = self
            .resident
            .current_binding_in(scope, "resultRegistryReceiver")
            .unwrap()
            .0;
        let receiver = self
            .resident
            .retain_binding_custody_in(scope, "resultRegistryReceiver", expected)
            .unwrap()
            .unwrap();
        let (activation, _) = suspended(
            self.run(|resident, settlement| {
                resident.run_rooted_application(
                    "result-fixture-receiver",
                    &receiver,
                    &payload,
                    RealmId::ROOT,
                    None,
                    settlement,
                )
            })
            .unwrap(),
        );
        let scope = self.context.placement.lexical_scope;
        let expected = self
            .resident
            .current_binding_in(scope, "resultRegistryScalar")
            .unwrap()
            .0;
        let reply = self
            .resident
            .retain_binding_custody_in(scope, "resultRegistryScalar", expected)
            .unwrap()
            .unwrap();
        let (publication, _) = suspended(
            self.run(|resident, settlement| resident.resume_handle(activation, reply, settlement))
                .unwrap(),
        );
        let captured = self
            .resident
            .capture_result_publication(&publication, site, RealmId::ROOT)
            .unwrap();
        assert!(matches!(
            self.run(|resident, settlement| resident.resume(publication, (), settlement))
                .unwrap(),
            ResidentOutcome::Completed { .. }
        ));
        captured
    }

    fn abort_submission(&mut self, hole: &ResidentHole, reason: &str) {
        let parked = self.resident.parked_count();
        assert!(matches!(
            self.resident.abort(hole.cont_id(), reason.to_owned()),
            Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(detail))))
                if detail == format!("ask aborted by caller: {reason}")
        ));
        assert_eq!(self.resident.parked_count(), parked - 1);
        assert!(!self.resident.parked_holes().contains(&hole.cont_id()));
    }

    fn force(&mut self, snapshot: &OwnedResultSnapshot) -> i64 {
        let scope = self.context.placement.lexical_scope;
        let expected = self
            .resident
            .current_binding_in(scope, "resultRegistryVerifier")
            .unwrap()
            .0;
        let verifier = self
            .resident
            .retain_binding_custody_in(scope, "resultRegistryVerifier", expected)
            .unwrap()
            .unwrap();
        let ResidentOutcome::Completed { result, .. } = self
            .run(|resident, settlement| {
                resident.run_rooted_application(
                    "force-retained-result",
                    &verifier,
                    snapshot.value(),
                    RealmId::ROOT,
                    None,
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("result force must complete")
        };
        result
            .to_json()
            .as_i64()
            .expect("native result force returns an Int")
    }
}

fn fixture() -> &'static parking_lot::Mutex<Fixture> {
    static FIXTURE: OnceLock<parking_lot::Mutex<Fixture>> = OnceLock::new();
    FIXTURE.get_or_init(|| parking_lot::Mutex::new(Fixture::new()))
}

/// Read metadata from the two actual compiler-attested submission frames.
/// Unexecuted library sites in the immutable program do not select requests.
pub(crate) fn request_signature_sites() -> Vec<tidepool_runtime::YieldSite> {
    let mut fixture = Fixture::with_source(include_str!("fixtures/request-signature-progress.hs"));
    let (submission, _, site) = fixture.submission(RequestId(51));
    fixture
        .resident
        .request_result_type_witness(site, &submission)
        .unwrap();
    let first = fixture
        .resident
        .parked_program_provenance(&submission)
        .unwrap()
        .site(site)
        .unwrap()
        .clone();
    let (reservation, _) = suspended(
        fixture
            .run(|resident, settlement| resident.resume(submission, Ok::<(), ()>(()), settlement))
            .unwrap(),
    );
    let (submission, payload) = suspended(
        fixture
            .run(|resident, settlement| resident.resume(reservation, Ok::<i64, ()>(52), settlement))
            .unwrap(),
    );
    let RepliesReq::SubmitRequestWith(_, site, _, _, _) =
        RepliesReq::from_value(&payload, fixture.resident.data_con_table()).unwrap()
    else {
        panic!("the second actual request must reach typed submission")
    };
    let site = u64::try_from(site).unwrap();
    fixture
        .resident
        .request_result_type_witness(site, &submission)
        .unwrap();
    let second = fixture
        .resident
        .parked_program_provenance(&submission)
        .unwrap()
        .site(site)
        .unwrap()
        .clone();
    fixture.abort_submission(&submission, "signature metadata observed");
    vec![first, second]
}

/// Reuse authentic immutable compiler products with a fresh mutable machine
/// and session library for each generated history and shrink replay.
pub(crate) fn fresh_history() {
    fixture().lock().reset();
}

pub(crate) fn admit_destination(registry: &RequestRegistry, owner: ActorRef, request: RequestId) {
    let destination = fixture().lock().destination(owner, request);
    registry
        .admit_result_destination(owner, request, destination)
        .unwrap();
}

#[test]
fn duplicate_destination_admission_preserves_the_first_issued_destination() {
    let registry = RequestRegistry::default();
    let owner = ActorRef::first(crate::ActorId(1));
    let target = ActorRef::first(crate::ActorId(2));
    let request = registry.reserve_for_operation(owner, target, "admission".into(), true, None);
    admit_destination(&registry, owner, request);
    let original = registry.state.lock().requests[&request]
        .operation()
        .unwrap()
        .destination()
        .unwrap();
    let duplicate = fixture().lock().destination(owner, request);
    assert_eq!(
        registry.admit_result_destination(owner, request, duplicate),
        Err(super::ReplyError::Stale)
    );
    registry.mark_queued(owner, target, request).unwrap();
    registry.present(target, request).unwrap();
    let claim = registry.begin_reply(target, request).unwrap();
    assert!(Arc::ptr_eq(&original, &claim.destination()));
    complete_reply(&registry, claim, None);
    assert_eq!(
        registry.observe_response(owner, request),
        Ok(super::ResponseObservation::Ready)
    );
}

/// Capture the real compiled installer at the public request preparation
/// boundary, before submission can publish any request input.
pub(crate) fn admit_reply_tool_request(
    registry: &RequestRegistry,
    owner: ActorRef,
    request: RequestId,
) {
    let _capture = capture_reply_tool_request(registry, owner, request);
}

fn capture_reply_tool_request(
    registry: &RequestRegistry,
    owner: ActorRef,
    request: RequestId,
) -> (Fixture, Arc<crate::request_reply_tool::ReplyToolDefinition>) {
    let mut fixture =
        Fixture::with_source_for_actor(include_str!("fixtures/reply-tool-request.hs"), owner);
    let producer = fixture.producer.clone();
    let (reservation, _) = suspended(
        fixture
            .run(|resident, settlement| {
                resident.run_with_sites("reply-tool-request", producer.code(), settlement)
            })
            .unwrap(),
    );
    let (registration, payload) = suspended(
        fixture
            .run(|resident, settlement| {
                resident.resume(
                    reservation,
                    Ok::<i64, ()>(request.0.try_into().unwrap()),
                    settlement,
                )
            })
            .unwrap(),
    );
    let RepliesReq::RegisterReplyToolWith(metadata, _, issued_request) =
        RepliesReq::from_value(&payload, fixture.resident.data_con_table()).unwrap()
    else {
        panic!("public JSON request must prepare its compiled reply before submission")
    };
    assert_eq!(u64::try_from(issued_request).unwrap(), request.0);
    let metadata = tidepool_runtime::value_to_json(&metadata, fixture.resident.data_con_table(), 0);
    let installer = fixture
        .resident
        .live_payload_handle_owned_by(registration.cont_id(), RealmId::ROOT)
        .unwrap()
        .expect("registration retains the compiled installer");
    // Exercise the production ExplicitLive ABI, rather than only retaining
    // an opaque payload that the recipient cannot subsequently invoke.
    let (installation, payload) = suspended(
        fixture
            .run(|resident, settlement| {
                resident.run_rooted_entry_borrowed(
                    "request_reply_installer",
                    &installer,
                    0,
                    RealmId::ROOT,
                    None,
                    settlement,
                )
            })
            .expect("request reply installer must implement the borrowed-entry function ABI"),
    );
    assert!(
        matches!(
            crate::generated::agent_tools::AgentToolsReq::from_value(
                &payload,
                fixture.resident.data_con_table(),
            )
            .unwrap(),
            crate::generated::agent_tools::AgentToolsReq::AgentToolsInstallWith(..)
        ),
        "the borrowed installer must enter the ordinary tool installation owner"
    );
    fixture.abort_submission(&installation, "borrowed reply installer observed");
    // Hosted invocation context is a source interpreter, composed alongside
    // the environment-owned actor primitives by the production facade.
    let mut support = crate::resident_workbench::intrinsic_effect_families();
    support.push(exomonad_tool::ToolEffectKey::ContextReadWrite);
    let mut execution =
        crate::request::sources::CallbackExecution::from_context(fixture.context.clone());
    execution.scope = Some(fixture.context.source_dependency().source_lease().unwrap());
    let definition = Arc::new(
        crate::request_reply_tool::ReplyToolDefinition::prepare(
            metadata,
            FIXTURE_SESSION,
            Arc::new(installer),
            execution,
            &support,
        )
        .unwrap(),
    );
    registry
        .register_reply_tool(owner, request, definition.clone())
        .unwrap();
    let (submission, payload) = suspended(
        fixture
            .run(|resident, settlement| resident.resume(registration, Ok::<(), ()>(()), settlement))
            .unwrap(),
    );
    let RepliesReq::SubmitRequestWith(_, site, _, _, _) =
        RepliesReq::from_value(&payload, fixture.resident.data_con_table()).unwrap()
    else {
        panic!("prepared reply tool must precede its typed submission")
    };
    let witness = fixture
        .resident
        .request_result_type_witness(u64::try_from(site).unwrap(), &submission)
        .unwrap();
    let destination = RequestResultDestination::new(
        owner,
        FIXTURE_SESSION,
        witness,
        fixture.resident.lease_bindings(&[]),
    );
    registry
        .admit_result_destination(owner, request, destination)
        .unwrap();
    fixture.abort_submission(
        &submission,
        "typed request prepared without mailbox publication",
    );
    (fixture, definition)
}

#[test]
fn reply_definition_retains_original_source_across_registration_and_recipient_transfer() {
    use crate::request::ReplyToolRegistrationError;
    use crate::resident_workbench::generation_admission::GenerationAdmissionError;
    let registry = RequestRegistry::default();
    let owner = ActorRef::first(crate::ActorId(101));
    let recipient = ActorRef::first(crate::ActorId(102));
    let request =
        registry.reserve_for_operation(owner, recipient, "reply source".into(), true, None);
    let (fixture, definition) = capture_reply_tool_request(&registry, owner, request);
    let source = fixture.context.source_dependency().source_lease().unwrap();
    assert!(definition.execution.snapshot.is_none());
    assert!(Arc::ptr_eq(
        &source,
        &definition
            .execution
            .source_dependency()
            .source_lease()
            .unwrap()
    ));
    let registered = registry.reply_tool(recipient, request).unwrap().unwrap();
    assert!(Arc::ptr_eq(&registered, &definition));
    let mut recipient_context = fixture.context.clone();
    recipient_context.actor = recipient;
    let transferred = definition.execution.relocated(recipient_context);
    assert!(transferred.snapshot.is_none());
    assert!(Arc::ptr_eq(
        &source,
        &transferred.source_dependency().source_lease().unwrap()
    ));
    let candidate = Arc::new(
        crate::request_reply_tool::ReplyToolDefinition::prepare(
            definition.installation.clone(),
            definition.session,
            definition.installer.clone(),
            transferred,
            &{
                let mut support = crate::resident_workbench::intrinsic_effect_families();
                support.push(exomonad_tool::ToolEffectKey::ContextReadWrite);
                support
            },
        )
        .unwrap(),
    );
    let late =
        registry.reserve_for_operation(recipient, owner, "late reply source".into(), true, None);
    let fence = source.source_admission().fence().unwrap();
    assert!(matches!(
        registry.register_reply_tool(recipient, late, candidate.clone()),
        Err(ReplyToolRegistrationError::Source(
            GenerationAdmissionError::Closed { .. }
        ))
    ));
    assert!(registry.reply_tool(owner, late).unwrap().is_none());
    assert!(Arc::ptr_eq(
        &registered,
        &registry.reply_tool(recipient, request).unwrap().unwrap()
    ));
    // The already registered decoder and ordinary request observation need no
    // fresh source authority, even though another callable registration refuses.
    assert_eq!(
        registered.schema().parameters(),
        definition.schema().parameters()
    );
    registry.mark_queued(owner, recipient, request).unwrap();
    registry.present(recipient, request).unwrap();
    assert_eq!(
        registry.observe_reply(recipient, request),
        Ok(super::ReplyObservation::Open)
    );
    assert_eq!(fence.pending_claims(), 0);
}

#[test]
fn terminal_drain_removes_reply_custody_and_refuses_republication() {
    use crate::resident_workbench::{
        BootstrapExpectation, BootstrapInstallationError, InstalledToolsState,
    };

    let registry = RequestRegistry::default();
    let owner = ActorRef::first(crate::ActorId(105));
    let recipient = ActorRef::first(crate::ActorId(106));
    let request = registry.reserve_for_operation(owner, recipient, "reply drain".into(), true, None);
    let (_fixture, definition) = capture_reply_tool_request(&registry, owner, request);
    let lease =
        crate::InstalledToolLease::new(owner, crate::CheckpointSourceLayer::default(), None);
    let activation = || {
        Arc::new(crate::RequestReplyActivation::custody_only(
            owner,
            request,
            lease.clone(),
            definition.clone(),
        ))
    };
    let state = InstalledToolsState::default();
    state
        .install_bootstrap(BootstrapExpectation::Empty, lease.clone())
        .unwrap();
    assert!(state.publish_reply(Some(activation())));
    assert!(state.active_reply().is_some());
    drop(state.drain_retired());
    assert!(
        state.active_reply().is_none(),
        "one terminal drain removes the reply-activation slot"
    );
    assert!(
        !state.publish_reply(Some(activation())),
        "a retired actor accepts no later reply custody"
    );
    assert!(state.active_reply().is_none());
    assert_eq!(
        state.install_bootstrap(BootstrapExpectation::Empty, lease),
        Err(BootstrapInstallationError::Retired)
    );
    assert!(state.snapshot_for_call().is_err());
}

pub(crate) fn complete_reply(
    registry: &RequestRegistry,
    claim: RequestReplyClaim,
    preview: Option<String>,
) -> Vec<WatchNotification> {
    let snapshot = snapshot(&claim);
    registry.finish_reply(claim, snapshot, preview)
}

/// Histories may attempt completion without an accepted reply. The driver can
/// commit only an affine token actually returned by the production registry.
pub(crate) fn complete_optional_reply(
    registry: &RequestRegistry,
    claim: &mut Option<RequestReplyClaim>,
    preview: Option<String>,
) -> Vec<WatchNotification> {
    claim
        .take()
        .map_or_else(Vec::new, |claim| complete_reply(registry, claim, preview))
}

pub(crate) fn publication(request: RequestId) -> RuntimeResultPublication {
    fixture().lock().publish(request)
}

pub(crate) fn snapshot(claim: &RequestReplyClaim) -> Arc<OwnedResultSnapshot> {
    OwnedResultSnapshot::incorporated(publication(claim.request()), claim.destination()).unwrap()
}

pub(crate) fn force(snapshot: &OwnedResultSnapshot) -> i64 {
    fixture().lock().force(snapshot)
}

pub(crate) fn outstanding_custody() -> usize {
    fixture().lock().resident.outstanding_custody()
}

pub(crate) fn command_report() -> tidepool_bridge_effects::CommandReport {
    tidepool_bridge_effects::CommandReport {
        command: vec!["true".into()],
        source: None,
        result: tidepool_bridge_effects::CommandResult {
            outcome: tidepool_bridge_effects::CommandOutcome::CommandExited(0),
            cleanup: tidepool_bridge_effects::CommandCleanup::CommandClean,
        },
        output_complete: true,
        tail: String::new(),
    }
}
