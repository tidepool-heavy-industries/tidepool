//! Real compiler-issued result roots reused across lifecycle histories.
//! Each test process compiles its immutable programs once; each reply executes a
//! fresh native request wrapper and captures its assembled ResponseResult.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use tidepool_bridge::{FromHaskell, HaskellValue};
use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{Generation, SessionId};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates,
    turn::{run_turn, CompiledTurn, TurnRequest, TurnResult},
    ModuleEnv, OutputSink, PersistentSession, ResidentHole, ResidentOutcome, ResidentSession,
    RuntimeResultPublication, SessionLib,
};
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
    _root: tempfile::TempDir,
    resident: Machine,
    producer: CompiledTurn,
}

fn suspended(outcome: ResidentOutcome) -> (ResidentHole, HaskellValue) {
    match outcome {
        ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        other => panic!("result fixture must suspend: {other:?}"),
    }
}

impl Fixture {
    fn new() -> Self {
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
            run_turn(TurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: source,
                templates: &templates,
                include: &paths,
                session_root: view.session_root(),
                inject_modules: &[],
                gen,
                verdict: None,
                target: None,
                retained_imports: &[],
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
        let producer = match compile(include_str!("fixtures/result-request.hs"), 2) {
            TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => compiled,
            TurnResult::Decl(_) => panic!("request fixture must execute"),
        };
        let library = SessionLib::open(
            FIXTURE_SESSION,
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(include);
        let mut resident = Machine::unbootstrapped(
            frunk::HNil,
            NoOutput,
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        resident.set_effect_execution(
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        assert!(matches!(
            resident
                .run_projected_bind_with_sites(
                    "result-fixture-native-bindings",
                    receiver.code(),
                    &bound,
                    Generation(1),
                )
                .unwrap(),
            ResidentOutcome::BindingsCommitted { .. }
        ));
        Self {
            _root: root,
            resident,
            producer,
        }
    }

    fn submission(&mut self, request: RequestId) -> (ResidentHole, HaskellValue, u64) {
        let (reservation, _) = suspended(
            self.resident
                .run_with_sites("result-fixture-request", self.producer.code())
                .unwrap(),
        );
        let (submission, payload) = suspended(
            self.resident
                .resume(
                    reservation,
                    Ok::<i64, ()>(i64::try_from(request.0).unwrap()),
                )
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
        self.resident
            .abort(hole.cont_id(), "fixture destination admitted".into())
            .unwrap();
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
        self.resident
            .abort(submission.cont_id(), "fixture payload transferred".into())
            .unwrap();
        let receiver = self
            .resident
            .retain_binding_custody("resultRegistryReceiver")
            .unwrap()
            .unwrap();
        let (activation, _) = suspended(
            self.resident
                .run_rooted_application(
                    "result-fixture-receiver",
                    &receiver,
                    &payload,
                    RealmId::ROOT,
                    None,
                )
                .unwrap(),
        );
        let reply = self
            .resident
            .retain_binding_custody("resultRegistryScalar")
            .unwrap()
            .unwrap();
        let (publication, _) = suspended(self.resident.resume_handle(activation, reply).unwrap());
        let captured = self
            .resident
            .capture_result_publication(&publication, site, RealmId::ROOT)
            .unwrap();
        assert!(matches!(
            self.resident.resume(publication, ()).unwrap(),
            ResidentOutcome::Completed { .. }
        ));
        captured
    }

    fn force(&mut self, snapshot: &OwnedResultSnapshot) -> i64 {
        let verifier = self
            .resident
            .retain_binding_custody("resultRegistryVerifier")
            .unwrap()
            .unwrap();
        let ResidentOutcome::Completed { result, .. } = self
            .resident
            .run_rooted_application(
                "force-retained-result",
                &verifier,
                snapshot.value(),
                RealmId::ROOT,
                None,
            )
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

pub(crate) fn admit_destination(registry: &RequestRegistry, owner: ActorRef, request: RequestId) {
    let destination = fixture().lock().destination(owner, request);
    registry
        .admit_result_destination(owner, request, destination)
        .unwrap();
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

pub(crate) fn snapshot(claim: &RequestReplyClaim) -> Arc<OwnedResultSnapshot> {
    OwnedResultSnapshot::incorporated(
        fixture().lock().publish(claim.request()),
        claim.destination(),
    )
    .unwrap()
}

pub(crate) fn force(snapshot: &OwnedResultSnapshot) -> i64 {
    fixture().lock().force(snapshot)
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
