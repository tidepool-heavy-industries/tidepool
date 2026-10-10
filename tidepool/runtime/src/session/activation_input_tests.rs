//! Real parked inputs through thin original-type issuance and affine mounting.

use super::*;
use crate::session::{
    insert_preamble_imports, resident_cell_check_template, resident_workbench_templates,
    turn::{self, run_turn, CompiledTurn, TurnRequest, TurnResult},
    ModuleEnv, SessionCompileView, SessionId,
};
use std::borrow::Cow;
use std::sync::Arc;
use tidepool_codegen::scope::ScopeId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_testing::effect_surface::TestEffectSurface;

#[derive(Clone)]
struct EmptyOutput;

impl OutputSink for EmptyOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }

    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

type TestSession = ResidentSession<frunk::HNil, EmptyOutput>;

/// The same owned source bytes and includes supply the admission and compiler.
struct InputRecipe {
    preamble: String,
    row: String,
    include: Vec<PathBuf>,
}

impl InputRecipe {
    fn digest(&self) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        for bytes in [self.preamble.as_bytes(), self.row.as_bytes()] {
            digest.update(&(bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        *digest.finalize().as_bytes()
    }
}

struct InputFixture {
    root: tempfile::TempDir,
    session: SessionId,
    recipe: Arc<InputRecipe>,
    producer: CompiledTurn,
}

impl InputFixture {
    fn compile(source: &str, opaque: bool, session: SessionId) -> Self {
        let (root, recipe) = Self::source_recipe(opaque);
        let library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(recipe.include.clone());
        let view = PersistentSession::new(Some(library), crate::DEFAULT_NURSERY_SIZE)
            .compile_view_in(ScopeId::ROOT)
            .unwrap();
        let producer = compiled(compile_turn(&view, &recipe, source, &[]));
        assert_startup_origin("producer", &producer, true);
        Self {
            root,
            session,
            recipe,
            producer,
        }
    }

    fn source_recipe(opaque: bool) -> (tempfile::TempDir, Arc<InputRecipe>) {
        tidepool_testing::eval_harness::require_extract();
        let effects = TestEffectSurface::minimal(&[
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
        .expect("minimal real AgentSession surface");
        let root = tempfile::tempdir().unwrap();
        let mut include = effects.include_paths().to_vec();
        include.push(PathBuf::from(
            std::env::var_os("TIDEPOOL_HASKELL_ACTORS_DIR")
                .expect("TIDEPOOL_HASKELL_ACTORS_DIR must name the declared actor source resource"),
        ));
        let mut preamble = effects.preamble().to_owned();
        for import in [
            "Tidepool.Agent.Reply (Replies, ResponseResult(..))",
            "Tidepool.Agent.Ref.Internal (AgentProtocol(..))",
            "qualified Tidepool.Agent.Ref.Internal as Ref",
            "qualified Tidepool.Actors.Internal.Agent as Agents",
            "qualified Tidepool.Effects.Core as Core",
        ] {
            preamble = insert_preamble_imports(&preamble, import);
        }
        if opaque {
            let home = root.path().join("home");
            std::fs::create_dir(&home).unwrap();
            std::fs::write(
                home.join("ActivationInputOriginal.hs"),
                include_str!("fixtures/activation-input-original.hs"),
            )
            .unwrap();
            std::fs::write(
                home.join("ActivationInputReplacement.hs"),
                include_str!("fixtures/activation-input-replacement.hs"),
            )
            .unwrap();
            include.push(home);
            for import in [
                "ActivationInputOriginal (Input)",
                "qualified ActivationInputOriginal",
                "qualified ActivationInputOriginal as Original",
            ] {
                preamble = insert_preamble_imports(&preamble, import);
            }
        }
        let recipe = Arc::new(InputRecipe {
            preamble,
            row: "'[Replies]".into(),
            include,
        });
        (root, recipe)
    }

    fn fresh(&self) -> TestSession {
        Self::fresh_in(self.session, &self.root, &self.recipe)
    }

    fn fresh_in(session: SessionId, root: &tempfile::TempDir, recipe: &InputRecipe) -> TestSession {
        let library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(recipe.include.clone());
        Self::fresh_with_library(library, recipe)
    }

    fn fresh_with_library(library: SessionLib, recipe: &InputRecipe) -> TestSession {
        Self::fresh_with_receiver(
            library,
            recipe,
            include_str!("fixtures/activation-input-receiver.hs"),
            2,
        )
    }

    fn fresh_with_receiver(
        library: SessionLib,
        recipe: &InputRecipe,
        receiver_source: &str,
        binding_count: usize,
    ) -> TestSession {
        let mut resident = TestSession::unbootstrapped(
            frunk::HNil,
            EmptyOutput,
            crate::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        resident
            .set_actor_execution(
                SessionRunContext::ROOT,
                EffectRunPolicy::SuspendAll,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .unwrap();
        let previous_context = resident.run_context();
        let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: execution.private_scope(),
                ..previous_context
            })
            .unwrap();
        let (bound, receiver, reservation) =
            compile_checked_binding(&mut resident, recipe, receiver_source, execution.clone());
        assert_eq!(
            bound.len(),
            binding_count,
            "declared native receiver bindings"
        );
        for name in ["activationReceiver", "activationUnitReply"] {
            assert_eq!(bound.iter().filter(|binder| binder.name == name).count(), 1);
        }
        if binding_count == 2 {
            assert!(
                receiver.asks.is_empty(),
                "pure receiver setup has no request sites"
            );
        }
        let checked_execution = receiver
            .certification
            .as_ref()
            .and_then(|certificate| certificate.checked_execution())
            .expect("receiver setup has its checked native output proof");
        assert!(checked_execution.matches_target(&receiver.prepared()));
        let interface = checked_execution
            .value_interface_certificate()
            .expect("receiver setup issued its exact value-interface certificate");
        assert_eq!(
            interface.owner(),
            tidepool_repr::SessionModule::val(reservation.generation())
        );
        let outcome = tidepool_testing::with_settlement(|settlement| {
            resident.run_projected_bind_with_sites(
                "nativeActivationReceiver",
                receiver.code(),
                &bound,
                reservation.generation(),
                settlement,
            )
        })
        .expect("install the checked native protocol receiver and Unit reply");
        assert!(
            matches!(outcome, ResidentOutcome::BindingsCommitted { .. }),
            "projected native binding completion: {outcome:?}"
        );
        publish_checked_fixture(&mut resident, &execution, binding_count);
        resident.set_run_context(previous_context).unwrap();
        resident.retire_scope(execution.private_scope());
        let retained = resident
            .state
            .retained_checked_value_artifact(interface.owner())
            .expect("receiver settlement retained the checked value-interface proof");
        assert!(Arc::ptr_eq(retained, &interface));
        let published = resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        for binder in &bound {
            assert!(
                published.bindings.iter().any(|(name, id)| {
                    name == &binder.name && *id == SessionVarId::from_extract(binder.var_id)
                }),
                "ROOT must select the original checked receiver binding ID"
            );
            assert!(resident
                .state
                .bindings()
                .get(SessionVarId::from_extract(binder.var_id))
                .is_some_and(|binding| binding.name.0 == binder.name));
        }
        resident
    }

    fn resume_activation(&self, resident: &mut TestSession, hole: ResidentHole) -> ResidentOutcome {
        let site = parked_site(resident, &hole);
        assert!(matches!(
            tidepool_testing::with_settlement(|settlement| resident.resume(hole.clone(), (), settlement)),
            Err(ResidentError::Prepared(PreparedRuntimeError::AnswerDelivery {
                site: rejected,
                delivery: tidepool_repr::execution_schema::SiteDelivery::ExitCellFill,
            })) if rejected == site
        ));
        assert!(resident.parked_holes().contains(&hole.cont_id()));
        let reply = resident
            .retain_binding_custody("activationUnitReply")
            .expect("retain the genuine native Unit reply")
            .expect("compiled Unit reply binding is installed");
        let publication = suspended(
            tidepool_testing::with_settlement(|settlement| {
                resident.resume_handle(hole, reply, settlement)
            })
            .expect("deliver native reply custody into the original request"),
        );
        let captured = resident
            .capture_result_publication(&publication, site, RealmId::ROOT)
            .expect("the protected helper publishes the original typed native result");
        let outcome = tidepool_testing::with_settlement(|settlement| {
            resident.resume(publication, (), settlement)
        })
        .expect("settle owned result publication");
        assert!(resident.discard_custody(captured.custody));
        outcome
    }

    fn start(&self, resident: &mut TestSession) -> ResidentHole {
        suspended(
            tidepool_testing::with_settlement(|settlement| {
                resident.run_with_sites(
                    "originalActivationRequests",
                    self.producer.code(),
                    settlement,
                )
            })
            .expect("execute authentic compiled request sites"),
        )
    }

    fn deliver(
        &self,
        resident: &mut TestSession,
        reservation: ResidentHole,
        request: i64,
    ) -> (ResidentHole, ResidentHole) {
        let submission = settle_request_reservation(resident, reservation, request);
        // The compiler attests the original site at field1 and protocol payload at field2.
        let payload = resident
            .live_payload_handle(submission.cont_id())
            .unwrap()
            .expect("original RunRequest remains a live native value");
        let receiver = resident
            .retain_binding_custody("activationReceiver")
            .unwrap()
            .expect("real compiled native receiver");
        let activation = suspended(
            tidepool_testing::with_settlement(|settlement| {
                resident.run_rooted_application(
                    "realRequestSession",
                    &receiver,
                    &payload,
                    RealmId::ROOT,
                    None,
                    settlement,
                )
            })
            .expect("execute original RunRequest through the typed receiver"),
        );
        let site = parked_site(resident, &activation);
        let provenance = resident.parked_program_provenance(&activation).unwrap();
        let metadata_matches = self
            .producer
            .asks
            .iter()
            .find(|original| original.site == site)
            .zip(provenance.sites.get(&site))
            .is_some_and(|(original, parked)| original.same_metadata(parked));
        eprintln!(
            "ACTIVATION_TRANSITION {}",
            serde_json::json!({
                "site": site,
                "payload_origin_authenticated": payload.provenance.authenticated_inputs.contains_key(&site),
                "parked_origin_authenticated": provenance.authenticated_inputs.contains_key(&site),
                "original_metadata_matches": metadata_matches,
            })
        );
        (submission, activation)
    }
}

fn publish_checked_fixture(
    resident: &mut TestSession,
    execution: &crate::session::PrivateExecutionAdmission,
    native_writes: usize,
) {
    let intent = resident
        .freeze_private_execution(execution)
        .expect("freeze the checked fixture publication");
    assert_eq!(intent.native_write_ids().len(), native_writes);
    let publication = resident
        .restage_ephemeral_execution_publication(intent)
        .expect("stage the checked fixture into ROOT");
    let ticket = match publication {
        crate::session::ExecutionPublication::Bindings(base) => base.stage().unwrap(),
        crate::session::ExecutionPublication::Declarations(base) => {
            let crate::session::CertifiedDeclarationPublication::Accepted(accepted) =
                tidepool_testing::with_settlement(|settlement| base.certify(settlement))
                    .expect("certify the checked declaration/value publication")
            else {
                panic!("checked fixture publication must be accepted");
            };
            accepted.stage().unwrap()
        }
    };
    assert_eq!(
        resident
            .publish_staged_public_manifest(ticket, &crate::session::PublicationDecision::new())
            .expect("commit the checked fixture to ROOT"),
        crate::session::PublicManifestCommit::Ephemeral,
    );
}

fn assert_startup_origin(label: &str, compiled: &CompiledTurn, requires_input: bool) {
    let proof = compiled
        .certification
        .as_ref()
        .and_then(|certification| certification.original_compile_input.as_ref());
    let matches = compiled
        .certification
        .as_ref()
        .is_some_and(|certification| {
            proof.is_some_and(|proof| {
                proof.matches_bundle(
                    &compiled.prepared(),
                    &certification.groups,
                    &certification.target_owners,
                    &certification.package_interfaces,
                    &compiled.table(),
                    &compiled.asks,
                )
            })
        });
    let sites = compiled
        .asks
        .iter()
        .map(|site| {
            serde_json::json!({
                "site": site.site,
                "inputs": site.inputs.len(),
                "input_witnesses": site.input_type_witnesses.len(),
                "first_input_witness": site.input_type_witnesses.first().and_then(Option::as_ref).map(|witness| witness.commitment()),
            })
        })
        .collect::<Vec<_>>();
    eprintln!(
        "ACTIVATION_ORIGIN {}",
        serde_json::json!({
            "stage": label,
            "certification_present": compiled.certification.is_some(),
            "original_compile_input_present": proof.is_some(),
            "matches_bundle": matches,
            "sites": sites,
        })
    );
    assert!(
        proof.is_some(),
        "{label}: startup compiler input proof absent"
    );
    assert!(matches, "{label}: startup compiler input bundle mismatch");
    if requires_input {
        assert!(
            compiled.asks.iter().any(|site| {
                site.input_type_witnesses.len() == site.inputs.len()
                    && site
                        .input_type_witnesses
                        .first()
                        .is_some_and(Option::is_some)
            }),
            "{label}: original canonical input witness absent"
        );
    }
}

fn compile_turn(
    view: &SessionCompileView,
    recipe: &InputRecipe,
    source: &str,
    retained: &[(tidepool_repr::execution_schema::SymbolIdentity, u64)],
) -> TurnResult {
    let imports = view.turn_imports(&SourceImports::new());
    let templates = resident_workbench_templates(&recipe.preamble, &recipe.row, &imports);
    let include = view.include_paths(&recipe.include);
    let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = view.injected_module_names();
    assert!(injected.is_empty(), "startup has no retained environment");
    assert!(retained.is_empty(), "startup has no retained native inputs");
    tidepool_testing::with_settlement(|settlement| {
        run_turn(
            TurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: source,
                templates: &templates,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                gen: view.next_value_generation().0,
                verdict: None,
                target: None,
                retained_imports: retained,
            },
            settlement,
        )
    })
    .expect("compile actual startup fixture")
}

fn compiled(result: TurnResult) -> CompiledTurn {
    match result {
        TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => compiled,
        TurnResult::Decl(_) => panic!("fixture must execute a real request or value probe"),
    }
}

fn parked_site(resident: &mut TestSession, hole: &ResidentHole) -> u64 {
    let id = resident
        .parked
        .iter()
        .find(|entry| entry.name == hole.cont_id())
        .unwrap()
        .id;
    resident
        .state
        .prepared_mut()
        .unwrap()
        .parked_site(id)
        .expect("activation fixture parks at a typed request site")
}

fn suspended(outcome: ResidentOutcome) -> ResidentHole {
    match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("expected native request suspension, got {other:?}"),
    }
}

// These fixtures exercise the admitted branch of Replies' Either answers.
// The unused error type supplies no value; native reply evidence validates
// Right and its payload against the compiler-issued reply type.
fn settle_request_reservation(
    resident: &mut TestSession,
    hole: ResidentHole,
    request: i64,
) -> ResidentHole {
    suspended(
        tidepool_testing::with_settlement(|settlement| {
            resident.resume(hole, Ok::<i64, ()>(request), settlement)
        })
        .expect("settle ReserveRequestWith with Right request ID"),
    )
}

fn settle_request_submission(resident: &mut TestSession, hole: ResidentHole) -> ResidentOutcome {
    tidepool_testing::with_settlement(|settlement| {
        resident.resume(hole, Ok::<(), ()>(()), settlement)
    })
    .expect("settle SubmitRequestWith with Right Unit")
}

type InputInterface = Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>;

fn issued_input(
    resident: &mut TestSession,
    hole: &ResidentHole,
    site: u64,
    recipe: Arc<InputRecipe>,
) -> (RuntimeActivationInputAdmission, InputInterface) {
    let realm = resident.parked_realm(hole).expect("original parked realm");
    let input = resident
        .capture_activation_input(hole, realm, site)
        .expect("claim original input and authenticated canonical type");
    issued_captured_input(resident, input, recipe)
}

fn issued_captured_input(
    resident: &mut TestSession,
    input: RuntimeActivationInput,
    recipe: Arc<InputRecipe>,
) -> (RuntimeActivationInputAdmission, InputInterface) {
    let scope = resident.run_context().lexical_scope;
    let witness = input.input_type_witness.clone();
    let before = resident.residency();
    let codegen = resident.codegen_totals();
    let visibility = resident.public_visibility_snapshot_in(scope).unwrap();
    let owner = resident
        .admit_activation_input_in(scope, input)
        .expect("reserve one original live input without authored checking");
    let reservation = owner.reservation();
    let requests = tidepool_toolchain::artifacts::host_binding_interface_request_count();
    let submissions = tidepool_extract_cmd::extract_spawn_count();
    let interface = tidepool_testing::with_settlement(|settlement| {
        tidepool_toolchain::artifacts::issue_host_binding_interface(
            reservation.prototype().clone(),
            reservation.digest(),
            reservation.generation().0,
            reservation.binding(),
            &recipe.include,
            settlement,
        )
    })
    .expect("issue only the original-type thin binding interface");
    assert_eq!(
        tidepool_toolchain::artifacts::host_binding_interface_request_count(),
        requests + 1,
    );
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), submissions + 1);
    assert_eq!(resident.residency(), before);
    assert_eq!(resident.codegen_totals(), codegen);
    assert_eq!(
        resident.public_visibility_snapshot_in(scope).unwrap(),
        visibility,
        "interface issuance cannot publish its reserved binder",
    );
    assert!(Arc::ptr_eq(interface.prototype(), reservation.prototype()));
    let original = interface
        .original_input_type()
        .expect("live-input purpose receipt");
    assert_eq!(original, witness.as_ref());
    assert_eq!(original.metadata_digest(), witness.metadata_digest());
    let binder = turn::decode_bound_binder(interface.binder()).unwrap();
    assert_eq!(binder.name, "sessionInput");
    assert!(
        binder.host_authority.is_none(),
        "live input has no host-builder authority"
    );
    (owner, interface)
}

fn mount_original(
    resident: &mut TestSession,
    owner: RuntimeActivationInputAdmission,
    interface: InputInterface,
) -> MountedActivationInput {
    let scope = resident.run_context().lexical_scope;
    let original_provenance = owner.input.custody.provenance.clone();
    let original_handle = owner.input.custody.handle.unwrap();
    let original_root = resident
        .state
        .require_prepared()
        .unwrap()
        .handle_slot(original_handle)
        .unwrap()
        .addr() as usize;
    let reservation = owner.reservation().clone();
    let certificate = interface.value_interface_certificate();
    let handles = resident.value_handle_count();
    let roots = resident.persistent_roots_count();
    let custody = resident.outstanding_custody();
    let codegen = resident.codegen_totals();
    let mounted = tidepool_testing::with_settlement(|settlement| {
        resident.mount_activation_input(owner, interface.clone(), settlement)
    })
    .expect("mount the original value under its thin type interface");
    assert!(resident
        .binding_names_in(scope)
        .iter()
        .any(|name| name == "sessionInput"));
    assert_eq!(resident.value_handle_count(), handles);
    assert_eq!(resident.persistent_roots_count(), roots);
    assert_eq!(resident.outstanding_custody(), custody - 1);
    let entry = resident.state.bindings().get(mounted.binding()).unwrap();
    assert_eq!(entry.value.handle.raw(), original_handle);
    assert_eq!(
        resident
            .state
            .require_prepared()
            .unwrap()
            .handle_slot(original_handle)
            .unwrap()
            .addr(),
        original_root as *mut *mut u8,
    );
    assert_eq!(
        resident.codegen_totals(),
        codegen,
        "mount installs no preview program"
    );
    assert!(Arc::ptr_eq(
        &resident.binding_provenance[&mounted.binding().raw()],
        &original_provenance,
    ));
    assert!(Arc::ptr_eq(
        resident
            .state
            .retained_checked_value_artifact(certificate.owner())
            .unwrap(),
        &certificate,
    ));
    assert!(
        matches!(
            resident
                .state
                .consume_binding_interface(&reservation, &interface),
            Err(SessionError::StaleStagedDeclaration),
        ),
        "a transferred input reservation cannot be consumed twice"
    );
    mounted
}

struct CheckedFixtureCell {
    checked: turn::CellCheck,
    prefix: Arc<crate::session::RuntimeCheckedPrefix>,
}

fn check_fixture_cell(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    source: &str,
    execution: Arc<crate::session::PrivateExecutionAdmission>,
    declaration_count: usize,
) -> CheckedFixtureCell {
    try_check_fixture_cell(resident, recipe, source, execution, declaration_count)
        .expect("check fixture bindings through runtime admission")
}

fn try_check_fixture_cell(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    source: &str,
    execution: Arc<crate::session::PrivateExecutionAdmission>,
    declaration_count: usize,
) -> Result<CheckedFixtureCell, turn::CellCheckFailure> {
    use crate::session::TemplateSelector;
    use tidepool_toolchain::checked_cell::CheckedCellSpecification;

    let scope = resident.run_context().lexical_scope;
    let view = resident
        .compile_view_in(scope)
        .unwrap()
        .with_scoped_injection();
    let imports = view.turn_imports(&SourceImports::new());
    let preamble = if declaration_count == 0 {
        recipe.preamble.clone()
    } else {
        recipe.preamble.replace(
            "module Expr where",
            &format!(
                "module {} where",
                resident.next_declaration_module().unwrap().module_name()
            ),
        )
    };
    let template = resident_cell_check_template(&preamble, &recipe.row, &imports);
    let templates = resident_workbench_templates(&recipe.preamble, &recipe.row, &imports);
    let specification = Arc::new(CheckedCellSpecification {
        admission_digest: [0; 32],
        cell_source: source.into(),
        template_source: template.clone(),
        turn_templates: templates
            .iter()
            .map(|template| {
                let kind = match template.kind {
                    TemplateSelector::Decl => "decl",
                    TemplateSelector::Bind => "bind",
                    TemplateSelector::BindDiscard => "binddiscard",
                    TemplateSelector::Expr => "expr",
                };
                (kind.into(), template.source.clone())
            })
            .collect(),
        injected_modules: view.injected_module_names(),
        reserved_declaration_modules: Vec::new(),
    });
    let includes = view.include_paths(&recipe.include);
    let plan = tidepool_testing::with_settlement(|settlement| {
        tidepool_toolchain::artifacts::parse_cell_plan(specification.clone(), &includes, settlement)
    })?;
    let admission = resident
        .admit_planned_cell_for_execution(
            execution,
            plan,
            specification.clone(),
            specification.specification_digest(),
            recipe.digest(),
            includes,
            None,
        )
        .expect("admit checked fixture bindings with their selected interfaces");
    let (checked, program) = tidepool_testing::with_settlement(|settlement| {
        turn::compile_cell_program_admitted(admission.clone(), settlement)
    })?;
    let prefix = resident
        .begin_cell_program(admission, program)
        .unwrap()
        .expect("compiled fixture has executable items");
    Ok(CheckedFixtureCell { checked, prefix })
}

impl CheckedFixtureCell {
    fn compile_binding(
        &self,
        resident: &mut TestSession,
        index: usize,
    ) -> (
        Vec<BoundBinder>,
        CompiledTurn,
        Arc<crate::session::RuntimeCheckedItemAdmission>,
    ) {
        let item = self.checked.checked_item(index).unwrap();
        let reservation = resident
            .admit_checked_item(self.prefix.clone(), item.clone())
            .unwrap();
        let TurnResult::Bind {
            bound, compiled, ..
        } = turn::consume_cell_program_item(reservation.clone())
            .expect("obtain admitted fixture bindings")
        else {
            panic!("fixture setup or value probe must be a checked bind");
        };
        (bound, compiled, reservation)
    }

    fn adopt_declaration(&self, resident: &mut TestSession) -> String {
        let item = self.checked.checked_item(0).unwrap();
        let owner = item
            .planned_declaration()
            .unwrap()
            .product()
            .owner()
            .module
            .clone();
        let reservation = resident
            .admit_checked_item(self.prefix.clone(), item)
            .unwrap();
        resident.adopt_checked_declaration(reservation).unwrap();
        owner
    }
}

fn compile_checked_binding(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    source: &str,
    execution: Arc<crate::session::PrivateExecutionAdmission>,
) -> (
    Vec<BoundBinder>,
    CompiledTurn,
    Arc<crate::session::RuntimeCheckedItemAdmission>,
) {
    check_fixture_cell(resident, recipe, source, execution, 0).compile_binding(resident, 0)
}

fn publish_fixture_declaration(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    source: &str,
    native_writes: usize,
) -> String {
    let previous_context = resident.run_context();
    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..previous_context
        })
        .unwrap();
    let checked = check_fixture_cell(resident, recipe, source, execution.clone(), 1);
    let owner = checked.adopt_declaration(resident);
    assert_eq!(checked.checked.items.len(), native_writes + 1);
    let mut retained: Vec<(
        BoundBinder,
        Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    )> = Vec::new();
    for index in 1..checked.checked.items.len() {
        let (bound, compiled, reservation) = checked.compile_binding(resident, index);
        assert_eq!(bound.len(), 1);
        let selected = compiled
            .certification
            .as_ref()
            .unwrap()
            .artifact_view
            .descriptors();
        for (_, prior) in &retained {
            use sha2::Digest;
            let original = prior.certified_interface();
            let interface = original.interface();
            let selected = selected
                .iter()
                .find(|descriptor| descriptor.id == original.artifact_id())
                .expect("next checked binding retains the actual completed value certificate");
            let interface_sha256: [u8; 32] =
                sha2::Sha256::digest(interface.interface_bytes()).into();
            assert_eq!(selected.owner.unit, interface.unit());
            assert_eq!(selected.owner.module, interface.module());
            assert_eq!(
                selected.producer_sha256,
                interface.toolchain_identity_sha256()
            );
            assert_eq!(selected.interface_sha256, interface_sha256);
        }
        let certificate = compiled
            .certification
            .as_ref()
            .unwrap()
            .checked_execution()
            .unwrap()
            .value_interface_certificate()
            .unwrap();
        let outcome = tidepool_testing::with_settlement(|settlement| {
            resident.run_bind_with_sites(
                &bound[0].name,
                compiled.code(),
                &bound[0],
                reservation.generation(),
                settlement,
            )
        })
        .unwrap();
        assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
        retained.push((bound[0].clone(), certificate));
    }
    publish_checked_fixture(resident, &execution, native_writes);
    resident.set_run_context(previous_context).unwrap();
    resident.retire_scope(execution.private_scope());
    let published = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    for (binder, certificate) in retained {
        assert!(published
            .bindings
            .iter()
            .any(|(name, id)| name == &binder.name
                && *id == SessionVarId::from_extract(binder.var_id)));
        assert!(Arc::ptr_eq(
            resident
                .state
                .retained_checked_value_artifact(certificate.owner())
                .unwrap(),
            &certificate
        ));
    }
    owner
}

fn original_value_probe(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    source: &str,
    value: i64,
) {
    let previous_context = resident.run_context();
    let original_bindings = resident.binding_names_in(ScopeId::ROOT);
    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    let private_scope = execution.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: private_scope,
            ..previous_context
        })
        .unwrap();
    // The actual checked bind evaluates the original input and its display.
    let source = format!("originalInputProbe <- pure (({source}), T.pack (P.show ({source})))");
    let (bound, compiled, reservation) =
        compile_checked_binding(resident, recipe, &source, execution);
    assert_eq!(bound.len(), 1);
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            resident.run_bind_with_sites(
                "originalInputProbe",
                compiled.code(),
                &bound[0],
                reservation.generation(),
                settlement,
            )
        })
        .expect("call mounted original input through its checked value interface")
    else {
        panic!("pure original-input probe must complete");
    };
    resident.set_run_context(previous_context).unwrap();
    resident.retire_scope(private_scope);
    assert_eq!(resident.binding_names_in(ScopeId::ROOT), original_bindings);
    assert_eq!(
        result.to_json(),
        serde_json::json!([value, value.to_string()])
    );
}

fn refuse_changed_checked_sites(resident: &mut TestSession, fixture: &InputFixture) {
    use crate::session::TemplateSelector;
    use tidepool_toolchain::checked_cell::CheckedCellSpecification;

    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    let scope = execution.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: scope,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let view = execution.view();
    let imports = view.turn_imports(&SourceImports::new());
    let template =
        resident_cell_check_template(&fixture.recipe.preamble, &fixture.recipe.row, &imports);
    let templates =
        resident_workbench_templates(&fixture.recipe.preamble, &fixture.recipe.row, &imports);
    let source = "let ordinaryValue = (42 :: Int)";
    let specification = Arc::new(CheckedCellSpecification {
        admission_digest: [0; 32],
        cell_source: source.into(),
        template_source: template.clone(),
        turn_templates: templates
            .iter()
            .map(|template| {
                let kind = match template.kind {
                    TemplateSelector::Decl => "decl",
                    TemplateSelector::Bind => "bind",
                    TemplateSelector::BindDiscard => "binddiscard",
                    TemplateSelector::Expr => "expr",
                };
                (kind.into(), template.source.clone())
            })
            .collect(),
        injected_modules: view.injected_module_names(),
        reserved_declaration_modules: Vec::new(),
    });
    let admission = resident
        .admit_planned_cell_for_execution(
            execution.clone(),
            tidepool_testing::with_settlement(|settlement| {
                tidepool_toolchain::artifacts::parse_cell_plan(
                    specification.clone(),
                    &(view.include_paths(&fixture.recipe.include)),
                    settlement,
                )
            })
            .unwrap(),
            specification.clone(),
            specification.specification_digest(),
            fixture.recipe.digest(),
            view.include_paths(&fixture.recipe.include),
            None,
        )
        .unwrap();
    let (checked, program) = tidepool_testing::with_settlement(|settlement| {
        turn::compile_cell_program_admitted(admission.clone(), settlement)
    })
    .unwrap();
    let item = checked.checked_item(0).unwrap();
    let prefix = resident
        .begin_cell_program(admission, program)
        .unwrap()
        .unwrap();
    let reservation = resident.admit_checked_item(prefix.clone(), item).unwrap();
    let TurnResult::Bind {
        bound, compiled, ..
    } = turn::consume_cell_program_item(reservation.clone()).unwrap()
    else {
        panic!("real checked bind must carry its original site map");
    };
    let before = resident.public_visibility_snapshot_in(scope).unwrap();
    let before_prefix = prefix.snapshot();
    let residency = resident.residency();
    let mut changed = compiled.code();
    let mut sites = changed.sites.to_vec();
    sites.push(
        fixture
            .producer
            .asks
            .iter()
            .find(|site| !site.inputs.is_empty())
            .unwrap()
            .clone(),
    );
    changed.sites = Cow::Owned(sites);
    let error = tidepool_testing::with_settlement(|settlement| {
        resident.run_bind_with_sites(
            "changedCheckedSiteMap",
            changed,
            &bound[0],
            reservation.generation(),
            settlement,
        )
    })
    .err()
    .expect("sealed ordinary site map must reject tampering");
    assert!(
        matches!(error, ResidentError::Session(SessionError::Compile(_))),
        "{error:?}"
    );
    assert_eq!(resident.residency(), residency);
    assert!(Arc::ptr_eq(&before_prefix, &prefix.snapshot()));
    assert_eq!(
        resident.public_visibility_snapshot_in(scope).unwrap(),
        before
    );
    assert!(!resident
        .binding_names_in(scope)
        .iter()
        .any(|name| name == "ordinaryValue"));
}

#[test]
fn resident_parcel_executes_evaluated_receive_value_after_producer_retirement() {
    use tidepool_repr::execution_schema::SiteDelivery;

    fn fresh(session: SessionId, root: &tempfile::TempDir, recipe: &InputRecipe) -> TestSession {
        let library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(recipe.include.clone());
        let mut resident = TestSession::unbootstrapped(
            frunk::HNil,
            EmptyOutput,
            crate::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        resident
            .set_actor_execution(
                SessionRunContext::ROOT,
                EffectRunPolicy::SuspendAll,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .unwrap();
        resident
    }

    tidepool_testing::eval_harness::require_extract();
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::actor_local_decl()]).unwrap();
    let recipe = InputRecipe {
        preamble: insert_preamble_imports(
            &insert_preamble_imports(surface.preamble(), "Tidepool.Actor (receive)"),
            "qualified Tidepool.Aeson.Value as Json",
        ),
        row: surface.row().into(),
        include: surface.include_paths().to_vec(),
    };
    let source_root = tempfile::tempdir().unwrap();
    let mut source = fresh(SessionId(1791), &source_root, &recipe);
    let source_view = source.compile_view_in(ScopeId::ROOT).unwrap();
    let TurnResult::Bind {
        bound, compiled, ..
    } = compile_turn(
        &source_view,
        &recipe,
        include_str!("fixtures/resident-receive-value.hs"),
        &[],
    )
    else {
        panic!("the genuine Eff value must compile as a binding");
    };
    let [binder] = bound.as_slice() else {
        panic!("the producer must bind one Eff value");
    };
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| source.run_bind_with_sites(
            "evaluatedReceiveValue",
            compiled.code(),
            binder,
            source_view.next_value_generation(),
            settlement
        ))
        .unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    let custody = source
        .retain_binding_custody("heldRequest")
        .unwrap()
        .unwrap();
    let original = Arc::clone(&custody.provenance);
    let sites = original.sites();
    let [issued] = sites.as_slice() else {
        panic!("the actual receive-value custody must retain its unique GHC-issued site");
    };
    let site = issued.site;
    assert!(
        compiled.asks.iter().any(|row| row.same_metadata(issued)),
        "binding custody retains genuine compiler observations"
    );

    // The receiver compiles only an identity runner and the live answer. It
    // has never executed or installed the producer's request definition.
    let receiver_root = tempfile::tempdir().unwrap();
    let mut receiver = fresh(SessionId(1792), &receiver_root, &recipe);
    let receiver_view = receiver.compile_view_in(ScopeId::ROOT).unwrap();
    let TurnResult::Bind {
        bound,
        compiled: runner,
        ..
    } = compile_turn(
        &receiver_view,
        &recipe,
        include_str!("fixtures/resident-receive-value-runner.hs"),
        &[],
    )
    else {
        panic!("the receiver must compile its real runner and answer bindings");
    };
    assert!(
        runner.asks.iter().all(|row| row.site != site),
        "the independent receiver did not issue the producer's receive site"
    );
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| receiver.run_projected_bind_with_sites(
            "receiveValueRunner",
            runner.code(),
            &bound,
            receiver_view.next_value_generation(),
            settlement
        ))
        .unwrap(),
        ResidentOutcome::BindingsCommitted { .. }
    ));

    let submissions = tidepool_extract_cmd::extract_spawn_count();
    let source_handles = source.value_handle_count();
    let parcel = source.export_custody(custody).unwrap();
    assert_eq!(source.value_handle_count(), source_handles - 1);
    assert!(Arc::ptr_eq(&parcel.provenance, &original));
    let images = parcel
        .native
        .images()
        .iter()
        .map(|image| Arc::downgrade(&image.image))
        .collect::<Vec<_>>();
    let emitting_images = parcel
        .native
        .images()
        .iter()
        .filter(|image| {
            image
                .image
                .definition_facts()
                .sites
                .iter()
                .any(|row| row.site == site && row.delivery == SiteDelivery::LiveReentry)
        })
        .count();
    // This census observes the production exporter. An Eff continuation or
    // static binding may retain code; no image is removed to force a data-only case.
    eprintln!(
        "receive custody parcel: images={}, emitting_images={emitting_images}, bytes={}",
        images.len(),
        parcel.bytes()
    );
    drop(compiled);
    drop(source);
    drop(source_root);
    assert!(images.iter().all(|image| image.upgrade().is_some()));

    let imported = receiver.import_parcel(parcel, RealmId::ROOT).unwrap();
    assert!(Arc::ptr_eq(&imported.provenance, &original));
    let runner = receiver
        .retain_binding_custody("runRequestValue")
        .unwrap()
        .unwrap();
    let hole = suspended(
        tidepool_testing::with_settlement(|settlement| {
            receiver.run_rooted_application(
                "importedReceiveValue",
                &runner,
                &imported,
                RealmId::ROOT,
                None,
                settlement,
            )
        })
        .expect("the production custody importer admits the original typed request"),
    );
    assert_eq!(parked_site(&mut receiver, &hole), site);
    let parked = receiver.parked_program_provenance(&hole).unwrap();
    assert!(parked.sites[&site].same_metadata(&original.sites[&site]));
    assert_eq!(receiver.parked_count(), 1);
    assert_eq!(receiver.stowed_roots_count(), 1);
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| receiver.resume_classified(
            hole.clone(),
            serde_json::json!("custody reply"),
            settlement
        )),
        Err(ResidentResumeError::Rejected(ResidentError::Prepared(
            PreparedRuntimeError::AnswerDelivery {
                delivery: SiteDelivery::LiveReentry,
                ..
            }
        )))
    ));
    assert_eq!(receiver.parked_holes(), vec![hole.cont_id()]);
    let answer = receiver
        .retain_binding_custody("custodyAnswer")
        .unwrap()
        .unwrap();
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            receiver.resume_handle(hole, answer, settlement)
        })
        .unwrap()
    else {
        panic!("the genuine typed live answer must complete the receive value");
    };
    assert_eq!(result.to_json(), serde_json::json!("custody reply"));
    assert!(receiver.parked_holes().is_empty());
    assert_eq!(receiver.stowed_roots_count(), 0);
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), submissions);
    assert!(receiver.discard_custody(imported));
    assert!(receiver.discard_custody(runner));
    assert_eq!(receiver.outstanding_custody(), 0);
}

#[test]
fn resident_parcel_preserves_original_authenticated_request_across_sessions() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-function.hs"),
        false,
        SessionId(1706),
    );
    let mut source = fixture.fresh();
    let reservation = fixture.start(&mut source);
    let submission = settle_request_reservation(&mut source, reservation, 1);
    let payload = source
        .live_payload_handle(submission.cont_id())
        .unwrap()
        .unwrap();
    let original = Arc::clone(&payload.provenance);
    assert!(
        !original.authenticated_inputs.is_empty(),
        "real compiler issuer authenticates input"
    );
    let observations_only = ProgramProvenance::from_sites(&fixture.producer.asks).unwrap();
    assert!(
        observations_only.authenticated_inputs.is_empty(),
        "public metadata construction cannot create input authority"
    );
    let shared = source.export_shared(&payload).unwrap();
    assert!(Arc::ptr_eq(&shared.provenance, &original));
    let destination_root = tempfile::tempdir().unwrap();
    let mut destination =
        InputFixture::fresh_in(SessionId(1707), &destination_root, &fixture.recipe);
    let wrong_owner = source
        .retain_binding_custody("activationReceiver")
        .unwrap()
        .unwrap();
    let before = destination.residency();
    assert!(matches!(
        destination.export_shared(&payload),
        Err(ResidentError::ForeignCustody)
    ));
    assert!(matches!(
        destination.export_custody(wrong_owner),
        Err(ResidentError::ForeignCustody)
    ));
    assert_eq!(
        destination.residency(),
        before,
        "foreign export cannot attach another owner's metadata"
    );
    let consumed = source.export_custody(payload).unwrap();
    assert!(Arc::ptr_eq(&consumed.provenance, &original));
    let imported = destination.import_parcel(shared, RealmId::ROOT).unwrap();
    let repeated = destination.import_parcel(consumed, RealmId::ROOT).unwrap();
    assert!(Arc::ptr_eq(&imported.provenance, &original));
    assert!(Arc::ptr_eq(&repeated.provenance, &original));
    drop(source);

    let receiver = destination
        .retain_binding_custody("activationReceiver")
        .unwrap()
        .unwrap();
    let activation = suspended(
        tidepool_testing::with_settlement(|settlement| {
            destination.run_rooted_application(
                "importedOriginalRequest",
                &receiver,
                &imported,
                RealmId::ROOT,
                None,
                settlement,
            )
        })
        .unwrap(),
    );
    let site = parked_site(&mut destination, &activation);
    let parked = destination.parked_program_provenance(&activation).unwrap();
    assert!(parked.sites[&site].same_metadata(&original.sites[&site]));
    assert_eq!(
        parked.authenticated_inputs[&site], original.authenticated_inputs[&site],
        "the original selected immutable interface view survives source retirement"
    );
    let realm = destination.parked_realm(&activation).unwrap();
    let retained_context = &original.authenticated_inputs[&site].types;
    assert!(
        retained_context.artifact_view().descriptors().is_empty(),
        "Int -> Int input and Unit reply have no home type interfaces"
    );
    let certification = fixture.producer.certification.as_ref().unwrap();
    let compiler_context = certification
        .original_compile_input
        .as_ref()
        .unwrap()
        .original_interface_context(
            &fixture.producer.prepared(),
            &certification.groups,
            &certification.target_owners,
            &certification.package_interfaces,
            &fixture.producer.table(),
            &fixture.producer.asks,
        )
        .unwrap();
    let producer = compiler_context.toolchain_identity_sha256();
    assert_ne!(producer, [0; 32]);
    assert_eq!(retained_context.toolchain_identity_sha256(), producer);
    assert_eq!(
        original.authenticated_inputs[&site]
            .execution
            .require_unique(site)
            .unwrap()
            .context
            .toolchain_identity_sha256(),
        producer
    );
    assert!(
        !original.authenticated_inputs[&site]
            .execution
            .require_unique(site)
            .unwrap()
            .context
            .artifact_view()
            .descriptors()
            .is_empty(),
        "native execution authority survives the package-only type projection"
    );
    let mut repeated_provenance = (*original).clone();
    repeated_provenance.merge(&original).unwrap();
    assert_eq!(repeated_provenance, *original);
    assert!(
        !compiler_context.artifact_view().descriptors().is_empty(),
        "the compiler proof retains its original home closure before type projection"
    );
    let mut conflicting = (*original).clone();
    conflicting.authenticated_inputs.insert(
        site,
        AuthenticatedInputContext::capture(
            compiler_context.clone(),
            original.authenticated_inputs[&site].execution.clone(),
        ),
    );
    assert!(
        matches!(
            repeated_provenance.merge(&conflicting),
            Err(ProgramProvenanceError::AuthenticatedInputTypeContext { site: rejected, .. })
                if rejected == site
        ),
        "same site metadata cannot replace its retained authenticated interface context"
    );
    assert_eq!(
        repeated_provenance, *original,
        "context collision is atomic"
    );
    let input = destination
        .capture_activation_input(&activation, realm, site)
        .unwrap();
    let observations = destination.request_site_type_evidence(site).unwrap();
    assert_ne!(&observations, input.type_evidence().as_ref());
    assert_ne!(
        observations.commitment(),
        input.type_evidence().commitment(),
        "request authority commitment includes original compiler authentication"
    );
    let repeated_evidence = observations
        .authenticate_request_types(
            original.sites[&site]
                .request_type_signatures
                .clone()
                .unwrap(),
            retained_context,
        )
        .unwrap();
    assert_eq!(&repeated_evidence, input.type_evidence().as_ref());
    assert_eq!(
        repeated_evidence.commitment(),
        input.type_evidence().commitment()
    );
    let request_context = input.type_evidence().compile_context(None).unwrap();
    assert_eq!(
        request_context.declarations().toolchain_identity_sha256(),
        producer
    );
    assert!(request_context
        .declarations()
        .artifact_view()
        .descriptors()
        .is_empty());
    let compile_context = input
        .type_evidence()
        .compile_context(Some(&compiler_context))
        .unwrap();
    assert_eq!(
        compile_context.declarations().toolchain_identity_sha256(),
        producer
    );
    assert_eq!(input.input_type(), original.sites[&site].inputs[0].ty);
    assert!(Arc::ptr_eq(&input.custody.provenance, &parked));
    assert!(destination.discard_custody(imported));
    assert!(destination.discard_custody(repeated));
    let mut remaining_holes = destination
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(remaining_holes.remove(activation.cont_id()));
    let parked_count = destination.parked_count();
    assert!(matches!(
        destination.abort(
            activation.cont_id(),
            "transfer qualification complete".into(),
        ),
        Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(reason))))
            if reason == "ask aborted by caller: transfer qualification complete"
    ));
    assert_eq!(
        destination
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>(),
        remaining_holes
    );
    assert_eq!(destination.parked_count(), parked_count - 1);
    assert!(destination.parked_program_provenance(&activation).is_none());
    drop(input);
    drop(receiver);
    assert_eq!(destination.outstanding_custody(), 0);
}

/// The compiler issues one stable imported request site into two original
/// outputs. Their unused overlap may compose; executing that site must not
/// guess which output's original instance environment should render its input.
#[test]
fn shared_request_site_composes_but_demands_unique_original_preview_context() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};

    let (root, base) = InputFixture::source_recipe(false);
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("ProvenanceSharedRequest.hs"),
        include_str!("fixtures/ProvenanceSharedRequest.hs"),
    )
    .unwrap();
    let mut include = base.include.clone();
    include.push(home);
    let recipe = Arc::new(InputRecipe {
        preamble: insert_preamble_imports(
            &insert_preamble_imports(&base.preamble, "Data.Text (Text)"),
            "qualified ProvenanceSharedRequest as Shared",
        ),
        row: base.row.clone(),
        include,
    });
    let session = SessionId(1791);
    let library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let view = PersistentSession::new(Some(library), crate::DEFAULT_NURSERY_SIZE)
        .compile_view_in(ScopeId::ROOT)
        .unwrap();
    let producer = compiled(compile_turn(
        &view,
        &recipe,
        include_str!("fixtures/provenance-shared-request-producer.hs"),
        &[],
    ));
    assert_startup_origin("shared-site producer", &producer, true);
    let fixture = InputFixture {
        root,
        session,
        recipe,
        producer,
    };
    let mut source = fixture.fresh();
    let reservation = fixture.start(&mut source);
    let submission = settle_request_reservation(&mut source, reservation, 1);
    let payload = source
        .live_payload_handle(submission.cont_id())
        .unwrap()
        .unwrap();
    let original = Arc::clone(&payload.provenance);
    let parcel = source.export_custody(payload).unwrap();
    let receiver_root = tempfile::tempdir().unwrap();
    let library = SessionLib::open(
        SessionId(1792),
        receiver_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(fixture.recipe.include.clone());
    let mut destination = InputFixture::fresh_with_receiver(
        library,
        &fixture.recipe,
        include_str!("fixtures/provenance-shared-request-receiver.hs"),
        3,
    );
    let receiver = destination
        .retain_binding_custody("activationReceiver")
        .unwrap()
        .unwrap();
    let receiver_original = Arc::clone(&receiver.provenance);
    let overlap = original
        .authenticated_inputs
        .iter()
        .filter_map(|(site, context)| {
            receiver_original
                .authenticated_inputs
                .get(site)
                .map(|other| (*site, context, other))
        })
        .collect::<Vec<_>>();
    assert!(
        !overlap.is_empty(),
        "the unused imported function carries its genuinely issued request site"
    );
    for (site, left, right) in &overlap {
        assert!(original.sites[site].same_metadata(&receiver_original.sites[site]));
        assert_eq!(left.type_identity, right.type_identity);
        assert_ne!(
            left.execution, right.execution,
            "different original output environments survive issuance"
        );
    }
    let mut composed = (*receiver_original).clone();
    composed
        .merge(&original)
        .expect("compatible unused sites cannot block rooted application");
    let (_, left, right) = overlap[0];
    let pool = [left.execution.clone(), right.execution.clone()];
    let fold = |indices: &[usize]| {
        let mut contexts = pool[indices[0]].clone();
        for index in &indices[1..] {
            contexts.merge(&pool[*index]);
        }
        contexts
    };
    let mut config = Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::shared_request_site_composes_but_demands_unique_original_preview_context"
    ));
    TestRunner::new(config)
        .run(
            &(proptest::collection::vec(0usize..2, 2..24), any::<usize>()),
            |(indices, split)| {
                // Independent list oracle: flatten captured identities, sort, dedup.
                let mut expected = indices
                    .iter()
                    .flat_map(|index| pool[*index].contexts().map(|context| context.identity))
                    .collect::<Vec<_>>();
                expected.sort();
                expected.dedup();
                let result = fold(&indices);
                let actual = result
                    .contexts()
                    .map(|context| context.identity)
                    .collect::<Vec<_>>();
                prop_assert_eq!(actual, expected);
                let mut reversed = indices.clone();
                reversed.reverse();
                prop_assert_eq!(&result, &fold(&reversed));
                let mut repeated = result.clone();
                repeated.merge(&result);
                prop_assert_eq!(&result, &repeated);
                let split = 1 + split % (indices.len() - 1);
                let mut associated = fold(&indices[..split]);
                associated.merge(&fold(&indices[split..]));
                prop_assert_eq!(result, associated);
                Ok(())
            },
        )
        .unwrap();
    let original_context = left.execution.contexts().next().unwrap();
    let duplicate = OriginalExecutionContexts::Unique(OriginalExecutionContext::capture(
        Arc::clone(&original_context.context),
    ));
    let mut repeated = left.execution.clone();
    repeated.merge(&duplicate);
    assert!(
        Arc::ptr_eq(repeated.contexts().next().unwrap(), original_context),
        "exact semantic duplicates retain the original context owner"
    );

    // Negative metadata tampering is observation-only: it cannot mint a native
    // site or input authority. A lower sorted row exposes partial insertion.
    let (site, _, _) = overlap[0];
    let mut conflicting = (*original).clone();
    let mut fresh = original.sites[&site].clone();
    fresh.site = 0;
    assert!(!receiver_original.sites.contains_key(&0));
    conflicting.sites.insert(0, fresh);
    conflicting
        .sites
        .get_mut(&site)
        .unwrap()
        .ty
        .push_str(" tampered");
    let mut unchanged = (*receiver_original).clone();
    assert!(matches!(
        unchanged.merge(&conflicting),
        Err(ProgramProvenanceError::SiteMetadata(_))
    ));
    assert_eq!(unchanged, *receiver_original);

    drop(source);
    let imported = destination.import_parcel(parcel, RealmId::ROOT).unwrap();
    let activation = suspended(
        tidepool_testing::with_settlement(|settlement| {
            destination.run_rooted_application(
                "sharedOriginalRequest",
                &receiver,
                &imported,
                RealmId::ROOT,
                None,
                settlement,
            )
        })
        .unwrap(),
    );
    let demanded = parked_site(&mut destination, &activation);
    assert!(
        overlap.iter().any(|(site, _, _)| *site == demanded),
        "the real continuation executes the shared site, not an unrelated constructed row"
    );
    let parked = destination.parked_program_provenance(&activation).unwrap();
    assert_eq!(*parked, composed);
    let expected = parked.authenticated_inputs[&demanded]
        .execution
        .contexts()
        .map(|context| context.identity)
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 2);
    let realm = destination.parked_realm(&activation).unwrap();
    let handles = destination.value_handle_count();
    let roots = destination.persistent_roots_count();
    let custody = destination.outstanding_custody();
    let holes = destination
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let visibility = destination.public_visibility_snapshot_in(ScopeId::ROOT);
    let submissions = tidepool_extract_cmd::extract_spawn_count();
    assert!(
        matches!(destination.capture_activation_input(&activation, realm, demanded),
        Err(ResidentError::AmbiguousActivationInputOriginalContext {site, contexts})
        if site == demanded && contexts == expected)
    );
    assert!(matches!(
        destination.capture_activation_input(&activation, realm, demanded.wrapping_add(1)),
        Err(ResidentError::InvalidActivationInput { .. })
    ));
    assert_eq!(destination.value_handle_count(), handles);
    assert_eq!(destination.persistent_roots_count(), roots);
    assert_eq!(destination.outstanding_custody(), custody);
    assert_eq!(
        destination
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        holes
    );
    assert_eq!(
        destination.public_visibility_snapshot_in(ScopeId::ROOT),
        visibility
    );
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        submissions,
        "ambiguity refuses before mount, preview compilation or display execution"
    );
    assert!(destination
        .state
        .bindings()
        .resolve("sessionInput")
        .is_none());
    assert!(matches!(
        fixture.resume_activation(&mut destination, activation),
        ResidentOutcome::Completed { .. }
    ));
    drop(imported);
    drop(receiver);
    assert_eq!(destination.outstanding_custody(), 0);
    assert_eq!(destination.parked_count(), 0);
}

#[test]
fn activation_authentication_follows_selected_native_sites_through_custody() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};

    let (root, base) = InputFixture::source_recipe(false);
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("ProvenanceSharedRequest.hs"),
        include_str!("fixtures/ProvenanceSharedRequest.hs"),
    )
    .unwrap();
    let mut include = base.include.clone();
    include.push(home);
    let recipe = Arc::new(InputRecipe {
        preamble: insert_preamble_imports(
            &base.preamble,
            "qualified ProvenanceSharedRequest as Shared",
        ),
        row: base.row.clone(),
        include,
    });
    let session = SessionId(1793);
    let mut source = InputFixture::fresh_in(session, &root, &recipe);
    let execution = Arc::new(source.begin_private_execution(ScopeId::ROOT).unwrap());
    source
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let checked = check_fixture_cell(
        &mut source,
        &recipe,
        include_str!("fixtures/activation-selected-site-segment.hs"),
        execution.clone(),
        0,
    );
    assert_eq!(checked.checked.items.len(), 4);
    let mut outputs = Vec::new();
    let mut values = Vec::new();
    for index in 0..4 {
        let (bound, compiled, reservation) = checked.compile_binding(&mut source, index);
        assert!(
            compiled
                .certification
                .as_ref()
                .and_then(|certification| certification.checked_execution())
                .is_some_and(|execution| execution.typed_entry().is_some()),
            "item {index} retains the complete compiler-issued typed entry"
        );
        let [binder] = bound.as_slice() else {
            panic!("each selected-site fixture item binds one value");
        };
        assert!(matches!(
            tidepool_testing::with_settlement(|settlement| source.run_bind_with_sites(
                "selectedSiteFixture",
                compiled.code(),
                binder,
                reservation.generation(),
                settlement
            ))
            .unwrap(),
            ResidentOutcome::Completed { .. }
        ));
        let binding = SessionVarId::from_extract(binder.var_id);
        assert_eq!(
            source
                .current_binding_in(execution.private_scope(), &binder.name)
                .unwrap()
                .0,
            binding,
            "native settlement owns the exact private binding"
        );
        assert!(source
            .current_binding_in(ScopeId::ROOT, &binder.name)
            .is_none());
        values.push(
            source
                .retain_binding_custody_in(execution.private_scope(), &binder.name, binding)
                .expect("retain the exact settled binding in its private scope")
                .expect("the checked native binding remains visible in its issuing scope"),
        );
        outputs.push(compiled);
    }

    let entries = outputs
        .iter()
        .map(|output| {
            output
                .certification
                .as_ref()
                .unwrap()
                .checked_execution()
                .unwrap()
                .typed_entry()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for entry in &entries[1..] {
        assert_eq!(entry.origin(), entries[0].origin());
        assert_eq!(entry.plan_digest(), entries[0].plan_digest());
        assert_ne!(entry.entry(), entries[0].entry());
    }

    // The fixture's shared action calls one OPAQUE home function. Its actual
    // original wire supplies the site ID; the other action's input is Int.
    // Neither expected membership set uses the provenance admission algorithm.
    let shared_sites = outputs[1]
        .certification
        .as_ref()
        .unwrap()
        .groups
        .iter()
        .filter(|group| group.owner().module == "ProvenanceSharedRequest")
        .flat_map(|group| group.group().definitions().sites())
        .filter(|site| !site.inputs.is_empty())
        .map(|site| site.site)
        .collect::<std::collections::BTreeSet<_>>();
    let local_sites = outputs[2]
        .asks
        .iter()
        .filter(|site| site.inputs.first().is_some_and(|input| input.ty == "Int"))
        .map(|site| site.site)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(shared_sites.len(), 1, "genuine imported native site");
    assert_eq!(local_sites.len(), 1, "genuine authored Int request site");
    assert!(shared_sites.is_disjoint(&local_sites));
    let expected = [
        std::collections::BTreeSet::new(),
        shared_sites.clone(),
        local_sites.clone(),
        std::collections::BTreeSet::new(),
    ];
    assert!(
        outputs[2]
            .certification
            .as_ref()
            .unwrap()
            .groups
            .iter()
            .flat_map(|group| group.group().definitions().sites())
            .any(|site| shared_sites.contains(&site.site)),
        "the later unrelated target retains earlier installed native custody"
    );
    let reobserved = outputs
        .iter()
        .map(|output| source.provenance_for(&output.code()).unwrap())
        .collect::<Vec<_>>();
    for (output, retained) in outputs.iter().zip(&reobserved) {
        let recomputed = CompiledProvenancePlan::build(&output.code())
            .unwrap()
            .instantiate();
        assert_eq!(
            retained, &recomputed,
            "retained plan agrees with fresh selected-fact recomputation"
        );
        for input in retained.authenticated_inputs.values() {
            let owner = input.execution.require_unique(0).unwrap();
            let independent = recomputed
                .authenticated_inputs
                .values()
                .find(|other| other.type_identity == input.type_identity)
                .unwrap()
                .execution
                .require_unique(0)
                .unwrap();
            assert!(
                !Arc::ptr_eq(&owner, &independent),
                "runtime execution owners remain fresh"
            );
        }
        let weak = recomputed
            .authenticated_inputs
            .values()
            .next()
            .map(|input| Arc::downgrade(&input.execution.require_unique(0).unwrap()));
        drop(recomputed);
        if let Some(weak) = weak {
            assert!(
                weak.upgrade().is_none(),
                "compiled plan never retains runtime execution owners"
            );
        }
    }
    let mut observed_unselected_request = false;
    for (index, (value, output)) in values.iter().zip(&outputs).enumerate() {
        assert_eq!(
            value
                .provenance
                .authenticated_inputs
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            expected[index],
            "item {index}: available originals do not select an executable site"
        );
        let selected = output
            .certification
            .as_ref()
            .unwrap()
            .checked_execution()
            .unwrap()
            .selected_native_sites()
            .expect("typed entries carry their sealed native-site selection");
        assert_eq!(
            &value.provenance.native_sites, selected,
            "settled custody retains exactly the issuer's selected native rows"
        );
        assert_eq!(
            &reobserved[index].native_sites, selected,
            "later installation cannot widen earlier native site authority"
        );
        assert_eq!(
            selected
                .ids()
                .filter(|site| shared_sites.contains(site) || local_sites.contains(site))
                .collect::<std::collections::BTreeSet<_>>(),
            expected[index],
            "issued selection follows the authored target, independently of installed custody"
        );
        assert_eq!(
            reobserved[index].authenticated_inputs, value.provenance.authenticated_inputs,
            "later native installation preserves the earlier target's exact authority"
        );
        // Each item retains its own compiler-issued census. Available native
        // owners and later items need not contribute metadata to every item.
        for site in &output.asks {
            assert!(
                value
                    .provenance
                    .sites
                    .get(&site.site)
                    .is_some_and(|retained| retained.same_metadata(site)),
                "issued site metadata remains unchanged on item {index}: {}",
                site.site,
            );
            observed_unselected_request |= (shared_sites.contains(&site.site)
                || local_sites.contains(&site.site))
                && !expected[index].contains(&site.site);
        }
    }
    assert!(
        observed_unselected_request,
        "the real compiler fixture exposes an unselected request site as metadata"
    );

    let mut config = Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::activation_authentication_follows_selected_native_sites_through_custody"
    ));
    let mut runner = TestRunner::new(config);
    runner
        .run(
            &prop::collection::vec((0_usize..4, any::<bool>()), 0..48),
            |history| {
                let mut composed = (*values[0].provenance).clone();
                let mut oracle = std::collections::BTreeSet::new();
                let mut native_oracle = values[0]
                    .provenance
                    .native_sites
                    .ids()
                    .collect::<std::collections::BTreeSet<_>>();
                for (index, after_installation) in history {
                    let observed = if after_installation {
                        &reobserved[index]
                    } else {
                        &values[index].provenance
                    };
                    composed.merge(observed).unwrap();
                    oracle.extend(expected[index].iter().copied());
                    native_oracle.extend(values[index].provenance.native_sites.ids());
                    prop_assert_eq!(
                        composed
                            .native_sites
                            .ids()
                            .collect::<std::collections::BTreeSet<_>>(),
                        native_oracle.clone()
                    );
                    prop_assert_eq!(
                        composed
                            .authenticated_inputs
                            .keys()
                            .copied()
                            .collect::<std::collections::BTreeSet<_>>(),
                        oracle.clone()
                    );
                    for site in &oracle {
                        let owner = if shared_sites.contains(site) { 1 } else { 2 };
                        prop_assert_eq!(
                            &composed.authenticated_inputs[site],
                            &values[owner].provenance.authenticated_inputs[site]
                        );
                    }
                }
                Ok(())
            },
        )
        .unwrap();

    let issued_request = values[1]
        .provenance
        .sites
        .values()
        .find(|site| shared_sites.contains(&site.site))
        .unwrap();
    let mut conflicting_observation = issued_request.clone();
    conflicting_observation.inputs.clear();
    conflicting_observation.input_type_witnesses.clear();
    let observed_only = ProgramProvenance::from_sites(&[conflicting_observation]).unwrap();
    assert!(
        observed_only.native_sites.ids().next().is_none(),
        "public sidecar construction cannot issue original native authority"
    );
    assert!(
        !observed_only.has_completion_site(issued_request.site),
        "an unsealed zero-input observation cannot issue completion authority"
    );
    for (left, right) in [
        (values[1].provenance.as_ref(), &observed_only),
        (&observed_only, values[1].provenance.as_ref()),
    ] {
        let mut combined = left.clone();
        let before = combined.clone();
        assert!(
            combined.merge(right).is_err(),
            "native input and observed completion cannot share one site id"
        );
        assert_eq!(combined, before, "refused authority composition is atomic");
    }

    let receiver_parcel = source.export_custody(values.remove(0)).unwrap();
    let action = values.remove(0);
    let reservation = suspended(
        tidepool_testing::with_settlement(|settlement| {
            source.run_rooted_entry("selectedAction", action, 1, RealmId::ROOT, None, settlement)
        })
        .unwrap(),
    );
    let holes_before_answer = source
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let handles_before_answer = source.value_handle_count();
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| source.resume(
            reservation.clone(),
            1_i64,
            settlement
        )),
        Err(ResidentError::Prepared(
            PreparedRuntimeError::AnswerConstructor { .. }
        ))
    ));
    assert_eq!(
        source
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        holes_before_answer
    );
    assert_eq!(source.value_handle_count(), handles_before_answer);
    let submission = settle_request_reservation(&mut source, reservation, 1);
    let payload = source
        .live_payload_handle(submission.cont_id())
        .unwrap()
        .unwrap();
    let original = Arc::clone(&payload.provenance);
    let payload_parcel = source.export_custody(payload).unwrap();
    let destination_root = tempfile::tempdir().unwrap();
    let mut destination = InputFixture::fresh_in(SessionId(1794), &destination_root, &recipe);
    let receiver = destination
        .import_parcel(receiver_parcel, RealmId::ROOT)
        .unwrap();
    let payload = destination
        .import_parcel(payload_parcel, RealmId::ROOT)
        .unwrap();
    assert!(receiver.provenance.authenticated_inputs.is_empty());
    assert!(Arc::ptr_eq(&payload.provenance, &original));
    assert_eq!(
        payload.provenance.native_sites, original.native_sites,
        "parcel export/import keeps original native site rows"
    );
    let activation = suspended(
        tidepool_testing::with_settlement(|settlement| {
            destination.run_rooted_application(
                "selectedOriginalRequest",
                &receiver,
                &payload,
                RealmId::ROOT,
                None,
                settlement,
            )
        })
        .unwrap(),
    );
    let site = parked_site(&mut destination, &activation);
    assert!(shared_sites.contains(&site));
    let input = destination
        .capture_activation_input(&activation, RealmId::ROOT, site)
        .unwrap();
    assert_eq!(
        input.original_execution.semantic_sha256(),
        original.authenticated_inputs[&site]
            .execution
            .require_unique(site)
            .unwrap()
            .context
            .semantic_sha256()
    );
    drop(input);
    let fixture = InputFixture {
        root,
        session,
        recipe,
        producer: outputs.remove(1),
    };
    assert!(matches!(
        fixture.resume_activation(&mut destination, activation),
        ResidentOutcome::Completed { .. }
    ));
    assert!(matches!(
        settle_request_submission(&mut source, submission),
        ResidentOutcome::Completed { .. }
    ));
    drop(receiver);
    drop(payload);
    drop(values);
    source.set_run_context(SessionRunContext::ROOT).unwrap();
    source.retire_scope(execution.private_scope());
    assert_eq!(source.outstanding_custody(), 0);
    assert_eq!(destination.outstanding_custody(), 0);
    assert!(source.parked_holes().is_empty());
    assert!(destination.parked_holes().is_empty());
}

#[test]
fn activation_function_input_preserves_value_across_repeated_checked_mounts() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-function.hs"),
        false,
        SessionId(1701),
    );
    let mut resident = fixture.fresh();
    let mut reservation = fixture.start(&mut resident);
    let mut previous_hole = None;
    for (request, value) in [(1, 42), (2, 43)] {
        let (submission, hole) = fixture.deliver(&mut resident, reservation, request);
        let site = parked_site(&mut resident, &hole);
        let wrong_site = fixture
            .producer
            .asks
            .iter()
            .find(|candidate| !candidate.inputs.is_empty() && candidate.site != site)
            .expect("other authentic request site")
            .site;
        let realm = resident.parked_realm(&hole).unwrap();
        let before = resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let custody = resident.outstanding_custody();
        if let Some(previous) = &previous_hole {
            assert!(matches!(
                resident.capture_activation_input(previous, realm, site),
                Err(ResidentError::InvalidActivationInput { site: rejected }) if rejected == site
            ));
        }
        assert!(matches!(
            resident.capture_activation_input(&hole, realm, wrong_site),
            Err(ResidentError::InvalidActivationInput { site: rejected }) if rejected == wrong_site
        ));
        assert_eq!(resident.outstanding_custody(), custody);
        assert_eq!(
            resident
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            before
        );
        let (owner, compiled) = issued_input(&mut resident, &hole, site, fixture.recipe.clone());
        mount_original(&mut resident, owner, compiled);
        original_value_probe(
            &mut resident,
            &fixture.recipe,
            "sessionInput (41 :: Int)",
            value,
        );
        previous_hole = Some(hole.clone());
        let outcome = fixture.resume_activation(&mut resident, hole);
        assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
        let caller = settle_request_submission(&mut resident, submission);
        if request == 1 {
            reservation = suspended(caller);
        } else {
            assert!(matches!(caller, ResidentOutcome::Completed { .. }));
            assert!(resident.parked_holes().is_empty());
            break;
        }
    }

    // Public site observations cannot issue input authority. Reuse the real
    // installed native closure and retain its custody, but carry only the
    // public sidecar across the value-to-code boundary.
    let mut unsealed = fixture.fresh();
    let reservation = fixture.start(&mut unsealed);
    let submission = settle_request_reservation(&mut unsealed, reservation, 1);
    let mut payload = unsealed
        .live_payload_handle(submission.cont_id())
        .unwrap()
        .expect("real original request payload");
    assert!(!payload.provenance.authenticated_inputs.is_empty());
    payload.provenance = Arc::new(ProgramProvenance::from_sites(&fixture.producer.asks).unwrap());
    assert!(payload.provenance.authenticated_inputs.is_empty());
    assert!(payload.provenance.native_sites.ids().next().is_none());
    let receiver = unsealed
        .retain_binding_custody("activationReceiver")
        .unwrap()
        .expect("real compiled request receiver");
    let hole = suspended(
        tidepool_testing::with_settlement(|settlement| {
            unsealed.run_rooted_application(
                "observedRequestWithoutInputAuthority",
                &receiver,
                &payload,
                RealmId::ROOT,
                None,
                settlement,
            )
        })
        .expect("native custody survives carrying public site observations"),
    );
    let site = parked_site(&mut unsealed, &hole);
    let provenance = unsealed.parked_program_provenance(&hole).unwrap();
    assert!(provenance.sites.contains_key(&site));
    assert!(!provenance.authenticated_inputs.contains_key(&site));
    drop(provenance);
    drop(receiver);
    drop(payload);
    let realm = unsealed.parked_realm(&hole).unwrap();
    let custody = unsealed.outstanding_custody();
    let before = unsealed
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    assert!(matches!(
        unsealed.capture_activation_input(&hole, realm, site),
        Err(ResidentError::UnauthenticatedActivationInputWitness { site: rejected })
            if rejected == site
    ));
    assert_eq!(unsealed.outstanding_custody(), custody);
    assert_eq!(
        unsealed
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap(),
        before
    );
    let mut owned_holes = [submission, hole]
        .into_iter()
        .map(|hole| hole.cont_id().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unsealed
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>(),
        owned_holes
    );
    assert_eq!(unsealed.close_realm(RealmId::ROOT), (0, 0));
    for id in owned_holes.clone() {
        assert!(matches!(
            unsealed.abort(&id, "release fixture-owned request".into(),),
            Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                _
            ))))
        ));
        assert!(owned_holes.remove(&id));
        assert_eq!(
            unsealed
                .parked_holes()
                .into_iter()
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>(),
            owned_holes
        );
    }

    let mut checked = fixture.fresh();
    refuse_changed_checked_sites(&mut checked, &fixture);
}

/// Resident declarations and captured checked values retain nominal owners across
/// lawful source shadowing. External-module type custody has a separate test.
#[test]
fn activation_opaque_input_native_owner_survives_same_spelling_source_shadow() {
    let (root, recipe) = InputFixture::source_recipe(false);
    let session = SessionId(1711);
    let mut resident = InputFixture::fresh_in(session, &root, &recipe);
    let original = publish_fixture_declaration(
        &mut resident,
        &recipe,
        include_str!("fixtures/activation-input-resident-original.hs"),
        2,
    );
    let original_head = resident
        .current_decl_heads_in(ScopeId::ROOT)
        .into_iter()
        .find(|(name, _)| name == "Input")
        .unwrap();
    let original_projection = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap()
        .bindings
        .into_iter()
        .find(|(name, _)| name == "originalProject")
        .unwrap()
        .1;
    let original_input = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap()
        .bindings
        .into_iter()
        .find(|(name, _)| name == "originalInput")
        .unwrap()
        .1;
    let previous_context = resident.run_context();
    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..previous_context
        })
        .unwrap();
    let (bound, producer, _) = compile_checked_binding(
        &mut resident,
        &recipe,
        include_str!("fixtures/activation-input-resident-request.hs"),
        execution.clone(),
    );
    assert!(
        bound.is_empty(),
        "request setup has no public output bindings"
    );
    assert!(producer
        .certification
        .as_ref()
        .unwrap()
        .checked_execution()
        .unwrap()
        .matches_target(&producer.prepared()));
    let fixture = InputFixture {
        root,
        session,
        recipe,
        producer,
    };
    let reservation = fixture.start(&mut resident);
    resident.set_run_context(previous_context).unwrap();
    let (submission, hole) = fixture.deliver(&mut resident, reservation, 1);
    let site = parked_site(&mut resident, &hole);
    let realm = resident.parked_realm(&hole).unwrap();
    let input = resident
        .capture_activation_input(&hole, realm, site)
        .unwrap();
    let shadow = publish_fixture_declaration(
        &mut resident,
        &fixture.recipe,
        include_str!("fixtures/activation-input-resident-shadow.hs"),
        0,
    );
    assert_ne!(
        original, shadow,
        "same spelling has a distinct nominal declaration owner"
    );
    let selected = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    for (name, id) in [
        ("originalProject", original_projection),
        ("originalInput", original_input),
    ] {
        assert!(
            selected
                .bindings
                .iter()
                .any(|(selected, actual)| selected == name && *actual == id),
            "shadowing preserves the actual original checked value ID"
        );
    }
    let shadow_head = resident
        .current_decl_heads_in(ScopeId::ROOT)
        .into_iter()
        .find(|(name, _)| name == "Input")
        .unwrap();
    assert_ne!(
        original_head, shadow_head,
        "ROOT publication advanced after lawful declaration shadow"
    );
    let (owner, compiled) = issued_captured_input(&mut resident, input, fixture.recipe.clone());
    mount_original(&mut resident, owner, compiled);
    original_value_probe(
        &mut resident,
        &fixture.recipe,
        "originalProject sessionInput",
        42,
    );
    assert_eq!(resident.outstanding_custody(), 0);
    assert!(matches!(
        fixture.resume_activation(&mut resident, hole),
        ResidentOutcome::Completed { .. }
    ));
    assert!(matches!(
        settle_request_submission(&mut resident, submission),
        ResidentOutcome::Completed { .. }
    ));
    resident.retire_scope(execution.private_scope());
    assert!(resident.parked_holes().is_empty());
}

#[test]
fn native_input_generation_retains_original_type_owner_after_source_readers_drop() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-opaque.hs"),
        true,
        SessionId(1712),
    );
    let inventory = fixture
        .producer
        .certification
        .as_ref()
        .expect("original sealed products")
        .artifact_view
        .inventory()
        .clone();
    let (original_unit, original_module, original_seal) = fixture
        .producer
        .asks
        .iter()
        .filter_map(|site| site.input_type_witnesses.first().and_then(Option::as_ref))
        .flat_map(|witness| witness.interface_seals())
        .find(|(_, module, _)| *module == "ActivationInputOriginal")
        .map(|(unit, module, seal)| (unit.to_owned(), module.to_owned(), seal.to_owned()))
        .expect("original input witness authenticates its private home type owner");
    let producer_descriptors = fixture
        .producer
        .certification
        .as_ref()
        .unwrap()
        .artifact_view
        .descriptors();
    let original = producer_descriptors
        .iter()
        .filter(|descriptor| {
            descriptor.kind
                == tidepool_toolchain::artifact_inventory::ArtifactKind::CanonicalModuleInterface
        })
        .find(|descriptor| {
            descriptor.owner.unit == original_unit && descriptor.owner.module == original_module
        })
        .cloned()
        .expect("compiler retained the actual original canonical home interface");
    assert_eq!(
        original
            .interface_sha256
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        original_seal,
        "canonical interface seal matches the original authenticated input witness"
    );
    let mut resident = fixture.fresh();
    let reservation = fixture.start(&mut resident);
    let (_submission, hole) = fixture.deliver(&mut resident, reservation, 1);
    let site = parked_site(&mut resident, &hole);
    let realm = resident.parked_realm(&hole).unwrap();
    let input = resident
        .capture_activation_input(&hole, realm, site)
        .unwrap();
    let evidence = input.type_evidence().clone();
    let recipe = Arc::new(InputRecipe {
        preamble: fixture
            .recipe
            .preamble
            .lines()
            .filter(|line| !line.contains("ActivationInputOriginal"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
        row: fixture.recipe.row.clone(),
        include: fixture
            .recipe
            .include
            .iter()
            .filter(|path| path.file_name().is_none_or(|name| name != "home"))
            .cloned()
            .collect(),
    });
    let InputFixture { root, producer, .. } = fixture;
    drop(producer);
    std::fs::remove_dir_all(root.path().join("home")).unwrap();
    let retained = evidence.compile_context(None).unwrap();
    let retained_descriptors = retained.declarations().artifact_view().descriptors();
    eprintln!(
        "ACTIVATION_TYPE_CUSTODY {}",
        serde_json::json!({
            "selected": original,
            "producer": producer_descriptors,
            "retained": retained_descriptors,
        })
    );
    assert!(
        retained_descriptors.contains(&original),
        "original canonical owner, producer and interface seals must survive: selected={original:?}, producer={producer_descriptors:?}, retained={retained_descriptors:?}"
    );
    assert_eq!(
        retained.declarations().toolchain_identity_sha256(),
        original.producer_sha256,
        "type context retains the canonical interface's authenticated producer"
    );
    assert!(
        retained.declarations().recovery_products().is_empty(),
        "type custody grants no original native products"
    );
    assert!(
        retained.declarations().lexical_graph().is_empty(),
        "type custody grants no source names"
    );
    drop(retained);
    assert!(inventory.node_count() > 0);

    let (owner, compiled) = issued_captured_input(&mut resident, input, recipe);
    let mounted = mount_original(&mut resident, owner, compiled);
    let view = resident.compile_view_in(ScopeId::ROOT).unwrap();
    assert!(
        view.exact_compile_context()
            .unwrap()
            .declarations()
            .artifact_view()
            .descriptors()
            .contains(&original),
        "the mounted interface retains its original type-only dependency",
    );
    drop(view);
    drop(mounted);
    assert!(inventory.node_count() > 0);
    drop(evidence);
    drop(resident);
    assert_eq!(
        inventory.node_count(),
        0,
        "the final native/value/request reader releases the original graph"
    );
}

/// Compile an ordinary request whose home module owns a custom display dictionary.
fn ordinary_home_display_fixture(session: SessionId) -> InputFixture {
    let (root, recipe) = InputFixture::source_recipe(false);
    let home = root.path().join("display-home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("ActivationDisplayOriginal.hs"),
        include_str!("fixtures/activation-input-display-original.hs"),
    )
    .unwrap();
    let mut include = recipe.include.clone();
    include.push(home);
    let recipe = Arc::new(InputRecipe {
        preamble: insert_preamble_imports(
            &recipe.preamble,
            "qualified ActivationDisplayOriginal as Original",
        ),
        row: recipe.row.clone(),
        include,
    });
    let library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let view = PersistentSession::new(Some(library), crate::DEFAULT_NURSERY_SIZE)
        .compile_view_in(ScopeId::ROOT)
        .unwrap();
    let producer = compiled(compile_turn(
        &view,
        &recipe,
        include_str!("fixtures/activation-input-opaque.hs"),
        &[],
    ));
    assert_startup_origin("ordinary custom display producer", &producer, true);
    InputFixture {
        root,
        session,
        recipe,
        producer,
    }
}

#[test]
fn activation_preview_keeps_original_display_with_retained_prefix_and_refuses_explicit_heap_inputs()
{
    use tidepool_toolchain::activation_preview::{
        ActivationPreviewSelection, ActivationPreviewSpecification,
    };
    use tidepool_toolchain::artifacts::ModuleCandidateOffer;
    use tidepool_toolchain::certified_products::PendingImportOwner;

    let fixture = ordinary_home_display_fixture(SessionId(1741));
    let mut resident = fixture.fresh();
    publish_fixture_declaration(
        &mut resident,
        &fixture.recipe,
        include_str!("fixtures/activation-preview-retained-prefix.hs"),
        2,
    );
    let unrelated_binding = resident
        .current_binding_in(ScopeId::ROOT, "unrelatedPreviewPrefix")
        .expect("genuine unrelated completed binding is present before request compilation");
    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let (bound, producer, _) = compile_checked_binding(
        &mut resident,
        &fixture.recipe,
        include_str!("fixtures/activation-preview-retained-request.hs"),
        execution.clone(),
    );
    assert!(bound.is_empty());
    let retained = producer
        .certification
        .as_ref()
        .unwrap()
        .groups
        .iter()
        .flat_map(|group| group.imports())
        .find_map(|import| match import {
            PendingImportOwner::Retained {
                identity,
                generation,
            } if identity.occurrence == "previewPrefix" => Some((identity.clone(), *generation)),
            _ => None,
        })
        .expect("actual request producer must retain its earlier live prefix binding");
    let producer_fixture = InputFixture {
        root: tempfile::tempdir().unwrap(),
        session: fixture.session,
        recipe: fixture.recipe.clone(),
        producer,
    };
    let reservation = producer_fixture.start(&mut resident);
    let (_, hole) = producer_fixture.deliver(&mut resident, reservation, 1);
    let site = parked_site(&mut resident, &hole);
    let input = resident
        .capture_activation_input(&hole, RealmId::ROOT, site)
        .unwrap();
    resident.set_run_context(SessionRunContext::ROOT).unwrap();
    let (owner, interface) = issued_captured_input(&mut resident, input, fixture.recipe.clone());
    let mounted = mount_original(&mut resident, owner, interface);
    let view = resident
        .compile_view_in(ScopeId::ROOT)
        .unwrap()
        .with_scoped_injection();
    let admission = resident.admit_activation_preview(mounted, view).unwrap();
    let template = turn::assemble_activation_preview_module(512);
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("ActivationPreviewTemplate.hs");
    std::fs::write(&source, &template).unwrap();
    let mut command = tidepool_extract_cmd::ExtractCmd::new().unwrap();
    command
        .input(&source)
        .turn()
        .activation_preview()
        .turn_out(scratch.path().join("turn.cbor"))
        .output_dir(scratch.path())
        .includes(&fixture.recipe.include)
        .session_root(admission.view().session_root())
        .session_incarnation(admission.view().session().0.to_string())
        .bind_gen(admission.generation().0);
    let endpoint = tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(
        command.bind().unwrap(),
    )
    .unwrap();
    let ActivationPreviewSelection::Ready(offer) = ModuleCandidateOffer::select_activation_preview(
        &endpoint,
        &fixture.recipe.include,
        scratch.path(),
        admission.exact_context().clone(),
        admission.input_interface().prototype().clone(),
        ActivationPreviewSpecification {
            original_context_digest: admission.original_context_digest(),
            budget: 512,
            template_source: template.clone(),
        },
        admission.catalog_selection(),
    )
    .unwrap() else {
        panic!("original display environment must be available");
    };
    if let Some(root) = offer.checked_value_root() {
        command.session_root(root);
    }
    let submissions = tidepool_extract_cmd::extract_spawn_count();
    offer
        .apply_to(&mut command)
        .expect("genuine preview offer accepts its original retained context");
    assert!(
        tidepool_extract_cmd::ExtractRequest::decode(&command.request_bytes())
            .unwrap()
            .retained_generations()
            .is_empty(),
        "successful pure preview transports no retained-generation fields"
    );
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), submissions);
    command.retained_generation(
        tidepool_extract_cmd::SymbolIdentity {
            unit: retained.0.unit,
            module: retained.0.module,
            namespace: retained.0.namespace,
            occurrence: retained.0.occurrence,
            record_parent: retained.0.record_parent,
        },
        retained.1,
    );
    let refused_request = command.request_bytes();
    assert!(
        offer.apply_to(&mut command).is_err(),
        "genuine preview offer refuses an explicit retained heap input"
    );
    assert_eq!(
        command.request_bytes(),
        refused_request,
        "explicit refusal cannot mutate the request"
    );
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), submissions);
    drop(offer);
    drop(endpoint);
    let handles = resident.value_handle_count();
    let visibility = resident.public_visibility_snapshot_in(ScopeId::ROOT);
    let compiled =
        match compile_activation_preview(admission, &template, 512, &fixture.recipe.include)
            .expect("pure preview must not transport unrelated original prefix generations")
        {
            turn::ActivationPreviewCompilation::Ready(compiled) => compiled,
            turn::ActivationPreviewCompilation::OriginalDisplayEvidenceUnavailable => {
                panic!("actual custom display remains available")
            }
        };
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            resident.run_activation_preview(compiled, settlement)
        })
        .unwrap()
    else {
        panic!("pure display must complete");
    };
    assert_eq!(
        result.to_json(),
        serde_json::json!(["original task display 42", false])
    );
    assert_eq!(resident.value_handle_count(), handles);
    assert_eq!(
        resident.public_visibility_snapshot_in(ScopeId::ROOT),
        visibility
    );
    assert_eq!(
        resident.current_binding_in(ScopeId::ROOT, "unrelatedPreviewPrefix"),
        Some(unrelated_binding)
    );
}

#[test]
fn activation_preview_executes_original_ordinary_home_custom_display_after_readers_drop() {
    use tidepool_toolchain::activation_preview::ActivationPreviewDisposition;
    let fixture = ordinary_home_display_fixture(SessionId(1739));
    let mut resident = fixture.fresh();
    let (owner, interface) = parked_input_owner(&fixture, &mut resident);
    let mounted = mount_original(&mut resident, owner, interface);
    let binding = mounted.binding();
    let original_context = mounted.original_execution.clone();
    let InputFixture {
        root: _session_root,
        recipe,
        producer,
        ..
    } = fixture;
    let child_home = tempfile::tempdir().unwrap();
    std::fs::write(
        child_home.path().join("ActivationDisplayOriginal.hs"),
        include_str!("fixtures/activation-input-display-shadow.hs"),
    )
    .unwrap();
    let mut includes = recipe.include.clone();
    includes.pop();
    includes.insert(0, child_home.path().to_path_buf());
    drop(producer);
    drop(recipe);
    let view = resident
        .compile_view_in(ScopeId::ROOT)
        .unwrap()
        .with_scoped_injection();
    let admission = resident.admit_activation_preview(mounted, view).unwrap();
    assert!(Arc::ptr_eq(
        admission.exact_context().declarations(),
        &original_context
    ));
    let handles = resident.value_handle_count();
    let visibility = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let template = turn::assemble_activation_preview_module(512);
    let retained_owner = admission.mounted.renderer_owner.clone();
    let compiled = match compile_activation_preview(admission, &template, 512, &includes)
        .expect("prepare a pure display from the original ordinary compiler evidence")
    {
        turn::ActivationPreviewCompilation::Ready(compiled) => compiled,
        turn::ActivationPreviewCompilation::OriginalDisplayEvidenceUnavailable => panic!(
            "the genuine ordinary home dictionary must retain complete original instance evidence"
        ),
    };
    assert_eq!(
        compiled.proof().disposition(),
        ActivationPreviewDisposition::Rendered
    );
    let ActivationRendererNative::Renderable(bundle) = &compiled.renderer.native else {
        panic!("renderable native custody")
    };
    let weak_images = bundle
        .image_owners()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    assert!(!weak_images.is_empty());
    let before =
        tidepool_codegen::prepared_program::CompiledProgram::successful_image_compilations();
    let repeated = super::super::prepared::NativeImageBundle::prepare_activation_renderer(
        &compiled.renderer.compiled,
        &resident.state.certified_image_registry(),
    )
    .unwrap();
    assert_eq!(
        before,
        tidepool_codegen::prepared_program::CompiledProgram::successful_image_compilations(),
        "same immutable code/literal keys reuse every native image"
    );
    eprintln!(
        "original ordinary-home display Ready bundle images: {}",
        bundle.image_owners().len()
    );
    for (first, next) in bundle.image_owners().zip(repeated.image_owners()) {
        assert!(Arc::ptr_eq(first, next));
    }
    drop(repeated);
    let before_fault = resident.residency();
    {
        let incomplete = bundle.omitting_target_image();
        let refused = resident.install_turn_program_in_with_images(
            compiled.admission.scope_lease.scope(),
            compiled.renderer.compiled.prepared().as_ref().clone(),
            compiled.renderer.compiled.certification.as_ref(),
            TurnImageAcquisition::Ready {
                bundle: &incomplete,
                table: compiled.renderer.compiled.table(),
            },
        );
        assert!(
            matches!(
                refused,
                Err(ResidentError::Prepared(
                    PreparedRuntimeError::MissingPreparedNativeImage
                ))
            ),
            "a registry hit outside complete Ready custody is refused"
        );
        assert_eq!(
            resident.residency(),
            before_fault,
            "no native program or mutable instance publishes on lookup refusal"
        );
        assert_eq!(
            before,
            tidepool_codegen::prepared_program::CompiledProgram::successful_image_compilations(),
            "lookup refusal never compiles"
        );
    }
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            resident.run_activation_preview(compiled, settlement)
        })
        .expect("execute the original custom dictionary against the original mounted heap input")
    else {
        panic!("a pure custom input display must complete without suspension");
    };
    assert_eq!(
        result.to_json(),
        serde_json::json!(["original task display 42", false])
    );
    assert!(
        resident.state.bindings().get(binding).is_some(),
        "preview keeps its committed input binding"
    );
    assert_eq!(
        resident.value_handle_count(),
        handles,
        "pure preview releases its temporary native source roots"
    );
    assert_eq!(
        resident.public_visibility_snapshot_in(ScopeId::ROOT),
        Some(visibility)
    );
    drop(resident);
    assert!(
        weak_images.iter().all(|image| image.upgrade().is_some()),
        "original renderer owner retains code after its machine exits"
    );
    drop(retained_owner);
    drop(original_context);
    assert!(
        weak_images.iter().all(|image| image.upgrade().is_none()),
        "last original renderer owner releases native code and literal storage"
    );
}

#[test]
fn activation_preview_package_only_input_preserves_original_producer_and_renders() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-package.hs"),
        false,
        SessionId(1741),
    );
    let mut resident = fixture.fresh();
    let reservation = fixture.start(&mut resident);
    let (_, hole) = fixture.deliver(&mut resident, reservation, 1);
    let site = parked_site(&mut resident, &hole);
    let input = resident
        .capture_activation_input(&hole, RealmId::ROOT, site)
        .unwrap();
    let producer = input.prototype.producer();
    assert!(input
        .prototype
        .context()
        .artifact_view()
        .descriptors()
        .is_empty());
    assert_ne!(producer, [0; 32]);
    let (owner, interface) = issued_captured_input(&mut resident, input, fixture.recipe.clone());
    assert_eq!(interface.prototype().producer(), producer);
    assert!(interface
        .prototype()
        .context()
        .artifact_view()
        .descriptors()
        .is_empty());
    assert_eq!(
        execute_mounted_input_preview(&mut resident, owner, interface, &fixture.recipe),
        serde_json::json!(["42", false]),
    );
}

#[test]
fn activation_preview_earlier_output_ignores_later_display_instance() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-opaque.hs"),
        true,
        SessionId(1742),
    );
    let mut resident = fixture.fresh();
    let reservation = fixture.start(&mut resident);
    let (_, hole) = fixture.deliver(&mut resident, reservation, 1);
    let site = parked_site(&mut resident, &hole);
    let earlier = resident
        .capture_activation_input(&hole, RealmId::ROOT, site)
        .unwrap();
    let recipe = Arc::new(InputRecipe {
        preamble: insert_preamble_imports(
            &insert_preamble_imports(
                &fixture.recipe.preamble,
                "Tidepool.Inspection.Display (Display(..), WorkbenchDisplay(..))",
            ),
            "qualified Data.Text as Text",
        ),
        row: fixture.recipe.row.clone(),
        include: fixture.recipe.include.clone(),
    });
    let display_owner = publish_fixture_declaration(
        &mut resident,
        &recipe,
        include_str!("fixtures/activation-input-later-display.hs"),
        0,
    );
    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let (bound, producer, _) = compile_checked_binding(
        &mut resident,
        &recipe,
        include_str!("fixtures/activation-input-opaque.hs"),
        execution.clone(),
    );
    assert!(bound.is_empty());
    // The checked output executes against the private view that admitted it.
    let later_fixture = InputFixture {
        root: tempfile::tempdir().unwrap(),
        session: fixture.session,
        recipe: recipe.clone(),
        producer,
    };
    let reservation = later_fixture.start(&mut resident);
    let (_, hole) = later_fixture.deliver(&mut resident, reservation, 2);
    let site = parked_site(&mut resident, &hole);
    let later = resident
        .capture_activation_input(&hole, RealmId::ROOT, site)
        .unwrap();
    assert!(earlier
        .original_execution
        .recovery_products()
        .iter()
        .all(|product| product.owner().module != display_owner));
    assert!(later
        .original_execution
        .recovery_products()
        .iter()
        .any(|product| product.owner().module == display_owner),
        "the original instance environment retains its defining native declaration, even when the request did not call Display");
    let earlier_identity = earlier.original_execution.semantic_sha256();
    let later_identity = later.original_execution.semantic_sha256();
    assert_ne!(earlier_identity, later_identity);
    let mut distinct = OriginalExecutionContexts::Unique(OriginalExecutionContext::capture(
        Arc::clone(&earlier.original_execution),
    ));
    distinct.merge(&OriginalExecutionContexts::Unique(
        OriginalExecutionContext::capture(Arc::clone(&later.original_execution)),
    ));
    let expected_contexts = std::collections::BTreeSet::from([earlier_identity, later_identity])
        .into_iter()
        .collect::<Vec<_>>();
    assert!(matches!(
        distinct.require_unique(site),
        Err(ResidentError::AmbiguousActivationInputOriginalContext { site: rejected, contexts })
            if rejected == site && contexts == expected_contexts
    ));
    resident.set_run_context(SessionRunContext::ROOT).unwrap();
    let (owner, interface) = issued_captured_input(&mut resident, earlier, recipe.clone());
    assert_eq!(
        execute_mounted_input_preview(&mut resident, owner, interface, &recipe),
        serde_json::json!(["<opaque>", false]),
        "the earlier output retains its original instance environment",
    );
    let (owner, interface) = issued_captured_input(&mut resident, later, recipe.clone());
    assert_eq!(
        execute_mounted_input_preview(&mut resident, owner, interface, &recipe),
        serde_json::json!(["later display 42", false]),
        "the later output must actually own the added instance",
    );
}

fn execute_mounted_input_preview(
    resident: &mut TestSession,
    owner: RuntimeActivationInputAdmission,
    interface: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
    recipe: &InputRecipe,
) -> serde_json::Value {
    let mounted = mount_original(resident, owner, interface);
    let binding = mounted.binding();
    let view = resident
        .compile_view_in(ScopeId::ROOT)
        .unwrap()
        .with_scoped_injection();
    let admission = resident.admit_activation_preview(mounted, view).unwrap();
    let handles = resident.value_handle_count();
    let visibility = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let template = turn::assemble_activation_preview_module(512);
    let compiled = match compile_activation_preview(admission, &template, 512, &recipe.include)
        .expect("prepare preview against actual original compiler authority")
    {
        turn::ActivationPreviewCompilation::Ready(compiled) => compiled,
        turn::ActivationPreviewCompilation::OriginalDisplayEvidenceUnavailable => {
            panic!("complete original display environment must remain available")
        }
    };
    assert_eq!(
        compiled.proof().disposition(),
        tidepool_toolchain::activation_preview::ActivationPreviewDisposition::Rendered,
        "the universal Display fallback is a real renderer, not missing-instance evidence",
    );
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            resident.run_activation_preview(compiled, settlement)
        })
        .unwrap()
    else {
        panic!("pure display must complete")
    };
    assert!(resident.state.bindings().get(binding).is_some());
    assert_eq!(
        resident.value_handle_count(),
        handles,
        "pure preview releases its temporary native source roots"
    );
    assert_eq!(
        resident.public_visibility_snapshot_in(ScopeId::ROOT),
        Some(visibility)
    );
    result.to_json()
}

#[test]
fn activation_preview_selected_original_dictionary_without_native_body_is_unavailable() {
    let (root, recipe) = InputFixture::source_recipe(true);
    let home = root.path().join("home");
    std::fs::write(
        home.join("ActivationDisplayUnavailable.hs"),
        include_str!("fixtures/activation-input-unavailable-display.hs"),
    )
    .unwrap();
    let recipe = Arc::new(InputRecipe {
        preamble: insert_preamble_imports(&recipe.preamble, "ActivationDisplayUnavailable ()"),
        row: recipe.row.clone(),
        include: recipe.include.clone(),
    });
    let session = SessionId(1743);
    let library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let view = PersistentSession::new(Some(library), crate::DEFAULT_NURSERY_SIZE)
        .compile_view_in(ScopeId::ROOT)
        .unwrap();
    let producer = compiled(compile_turn(
        &view,
        &recipe,
        include_str!("fixtures/activation-input-opaque.hs"),
        &[],
    ));
    assert_startup_origin("unused foreign-export dictionary owner", &producer, true);
    let fixture = InputFixture {
        root,
        session,
        recipe,
        producer,
    };
    let mut resident = fixture.fresh();
    let (owner, interface) = parked_input_owner(&fixture, &mut resident);
    let mounted = mount_original(&mut resident, owner, interface);
    let binding = mounted.binding();
    let dictionary_owner = mounted
        .original_execution
        .artifact_view()
        .descriptors()
        .into_iter()
        .find(|owner| owner.owner.module == "ActivationDisplayUnavailable")
        .expect("actual original output retains the dictionary owner's canonical interface");
    assert_eq!(
        dictionary_owner.kind,
        tidepool_toolchain::artifact_inventory::ArtifactKind::CanonicalModuleInterface
    );
    assert!(dictionary_owner.product_sha256.is_none());
    let view = resident
        .compile_view_in(ScopeId::ROOT)
        .unwrap()
        .with_scoped_injection();
    let admission = resident.admit_activation_preview(mounted, view).unwrap();
    let roots = resident.persistent_roots_count();
    let visibility = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let submissions = tidepool_extract_cmd::extract_spawn_count();
    let selected = compile_activation_preview(
        admission,
        &turn::assemble_activation_preview_module(512),
        512,
        &fixture.recipe.include,
    )
    .expect(
        "selected dictionary lacking native original Core has an authenticated unavailable result",
    );
    match selected {
        turn::ActivationPreviewCompilation::OriginalDisplayEvidenceUnavailable => {},
        turn::ActivationPreviewCompilation::Ready(compiled) => panic!(
            "original dictionary {} has no native body, but preview returned Ready with {:?} disposition",
            dictionary_owner.owner.module,
            compiled.proof().disposition(),
        ),
    }
    assert!(
        tidepool_extract_cmd::extract_spawn_count() > submissions,
        "the actual compiler probed and selected the original dictionary"
    );
    assert_eq!(resident.persistent_roots_count(), roots);
    assert_eq!(
        resident.public_visibility_snapshot_in(ScopeId::ROOT),
        Some(visibility)
    );
    assert!(resident.state.bindings().get(binding).is_some());
}

#[test]
fn activation_preview_type_only_original_context_is_unavailable_before_instance_probe() {
    use tidepool_toolchain::activation_preview::{
        ActivationPreviewSelection, ActivationPreviewSpecification,
    };
    use tidepool_toolchain::declaration_join::ExactCompileContext;
    let fixture = ordinary_home_display_fixture(SessionId(1740));
    let mut resident = fixture.fresh();
    let (owner, interface) = parked_input_owner(&fixture, &mut resident);
    let mounted = mount_original(&mut resident, owner, interface);
    let binding = mounted.binding();
    let type_only = mounted.interface().prototype().context().clone();
    assert!(type_only.recovery_products().is_empty());
    assert!(type_only.lexical_graph().is_empty());
    assert_eq!(
        type_only.toolchain_identity_sha256(),
        mounted.original_execution.toolchain_identity_sha256()
    );
    let view = resident
        .compile_view_in(ScopeId::ROOT)
        .unwrap()
        .with_scoped_injection();
    let admission = resident.admit_activation_preview(mounted, view).unwrap();
    let command = tidepool_extract_cmd::ExtractCmd::new().unwrap();
    let endpoint = tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(
        command.bind().unwrap(),
    )
    .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let submissions = tidepool_extract_cmd::extract_spawn_count();
    let roots = resident.persistent_roots_count();
    let visibility = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let selected = tidepool_toolchain::artifacts::ModuleCandidateOffer::select_activation_preview(
        &endpoint,
        &fixture.recipe.include,
        scratch.path(),
        Arc::new(ExactCompileContext::new(type_only)),
        admission.input_interface().prototype().clone(),
        ActivationPreviewSpecification {
            original_context_digest: admission.original_context_digest(),
            budget: 512,
            template_source: turn::assemble_activation_preview_module(512),
        },
        admission.catalog_selection(),
    )
    .expect("genuine type-only authority may report missing original instance evidence");
    assert!(
        matches!(
            selected,
            ActivationPreviewSelection::OriginalDisplayEvidenceUnavailable
        ),
        "type-only evidence cannot prove Opaque or select child display code"
    );
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        submissions,
        "no incomplete instance solver was invoked"
    );
    assert!(!scratch.path().join("turn.cbor").exists());
    assert_eq!(resident.persistent_roots_count(), roots);
    assert_eq!(
        resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .as_ref(),
        Some(&visibility)
    );
    assert!(resident.state.bindings().get(binding).is_some());
}

#[test]
fn activation_task_input_preserves_parent_action_row_across_child_facade() {
    let (root, recipe) = InputFixture::source_recipe(false);
    let session = SessionId(1736);
    let mut resident = InputFixture::fresh_in(session, &root, &recipe);
    publish_fixture_declaration(
        &mut resident,
        &recipe,
        include_str!("fixtures/activation-input-resident-task.hs"),
        3,
    );
    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let (bound, producer, _) = compile_checked_binding(
        &mut resident,
        &recipe,
        include_str!("fixtures/activation-input-resident-request.hs"),
        execution.clone(),
    );
    assert!(bound.is_empty());
    let fixture = InputFixture {
        root,
        session,
        recipe,
        producer,
    };
    let reservation = fixture.start(&mut resident);
    resident.set_run_context(SessionRunContext::ROOT).unwrap();
    let (submission, hole) = fixture.deliver(&mut resident, reservation, 1);
    let site = parked_site(&mut resident, &hole);
    let realm = resident.parked_realm(&hole).unwrap();
    let input = resident
        .capture_activation_input(&hole, realm, site)
        .unwrap();

    let child_effects = TestEffectSurface::minimal(&[]).unwrap();
    let child_recipe = Arc::new(InputRecipe {
        preamble: child_effects.preamble().to_owned(),
        row: child_effects.row().to_owned(),
        include: child_effects.include_paths().to_vec(),
    });
    assert_ne!(child_recipe.row, fixture.recipe.row);
    let (owner, interface) = issued_captured_input(&mut resident, input, child_recipe.clone());
    mount_original(&mut resident, owner, interface);
    original_value_probe(
        &mut resident,
        &child_recipe,
        "originalProject sessionInput",
        42,
    );

    let action_execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: action_execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let source = "originalActionResult <- originalAction sessionInput";
    let before = resident
        .public_visibility_snapshot_in(action_execution.private_scope())
        .unwrap();
    let failure = try_check_fixture_cell(
        &mut resident,
        &child_recipe,
        source,
        action_execution.clone(),
        0,
    )
    .err()
    .expect("captured parent action cannot be retyped to the child empty effect row");
    assert!(
        matches!(failure.error, crate::CompileError::Diagnostics(_)),
        "{failure:?}"
    );
    assert_eq!(
        resident
            .public_visibility_snapshot_in(action_execution.private_scope())
            .unwrap(),
        before
    );
    assert!(!resident
        .binding_names_in(action_execution.private_scope())
        .iter()
        .any(|name| name == "originalActionResult"));
    resident.set_run_context(SessionRunContext::ROOT).unwrap();
    resident.retire_scope(action_execution.private_scope());

    let action_execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: action_execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let (bound, compiled, reservation) = compile_checked_binding(
        &mut resident,
        &fixture.recipe,
        source,
        action_execution.clone(),
    );
    let [binder] = bound.as_slice() else {
        panic!("parent action has one checked result");
    };
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            resident.run_bind_with_sites(
                "originalActionResult",
                compiled.code(),
                binder,
                reservation.generation(),
                settlement,
            )
        })
        .expect("execute the original captured closure under its original permitted effect row")
    else {
        panic!("pure parent action must complete");
    };
    assert_eq!(result.to_json(), serde_json::json!(43));
    resident.set_run_context(SessionRunContext::ROOT).unwrap();
    resident.retire_scope(action_execution.private_scope());
    assert!(matches!(
        fixture.resume_activation(&mut resident, hole),
        ResidentOutcome::Completed { .. }
    ));
    assert!(matches!(
        settle_request_submission(&mut resident, submission),
        ResidentOutcome::Completed { .. }
    ));
    resident.retire_scope(execution.private_scope());
    assert!(resident.parked_holes().is_empty());
}

fn parked_input_owner(
    fixture: &InputFixture,
    resident: &mut TestSession,
) -> (RuntimeActivationInputAdmission, InputInterface) {
    let reservation = fixture.start(resident);
    let (_, hole) = fixture.deliver(resident, reservation, 1);
    let site = parked_site(resident, &hole);
    issued_input(resident, &hole, site, fixture.recipe.clone())
}

#[test]
fn durable_activation_binding_retains_private_authored_source_after_retirement_and_collection() {
    use crate::session::{PublicManifestCommit, PublicationDecision, RecoveryPublicOwner};
    struct RunOwner {
        root: PathBuf,
        _lock: std::fs::File,
    }
    impl crate::session::RecoveryRunAuthority for RunOwner {
        fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
            Ok(root.canonicalize()? == self.root)
        }
    }
    let (root, recipe) = InputFixture::source_recipe(false);
    let session = SessionId(1741);
    let mut library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let durable_root = tempfile::tempdir().unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(durable_root.path().join("run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    // Receiver setup reserves a Join identity even for a binding-only
    // publication. Attach recovery before that allocator can advance.
    assert_eq!(library.generation(), Generation(0));
    tidepool_testing::with_settlement(|settlement| {
        library.attach_owned_recovery_graph_v3(
            durable_root.path().join("declarations.json"),
            Arc::new(RunOwner {
                root: durable_root.path().canonicalize().unwrap(),
                _lock: lock,
            }),
            settlement,
        )
    })
    .unwrap_or_else(|error| panic!("fresh declaration library must attach recovery: {error:?}"));
    let mut resident = InputFixture::fresh_with_library(library, &recipe);
    assert_eq!(resident.state.lib().scope_tip(ScopeId::ROOT), Generation(0));
    assert_eq!(
        resident
            .state
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .graph
            .high_water(),
        resident.state.lib().generation(),
    );
    let public = resident.mint_scope(ScopeId::ROOT).unwrap();
    let durable = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/private-source-input").unwrap(),
        1,
    )
    .unwrap();
    assert_eq!(
        resident
            .initialize_durable_public_scope(durable.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let public_context = SessionRunContext {
        lexical_scope: public,
        ..SessionRunContext::ROOT
    };
    resident.set_run_context(public_context).unwrap();
    let before = resident.public_visibility_snapshot_in(public).unwrap();
    let before_binding_ids = resident
        .state
        .bindings()
        .scope_reachable_binding_ids(resident.state.scope_tree(), public);
    let before_selection = resident
        .state
        .bindings()
        .source_domain_selection_in(resident.state.scope_tree(), public)
        .unwrap();
    assert!(before_selection.inherited().is_empty());
    let before_binding_tip = resident.state.bindings().tip_id(public);

    let producer_execution = Arc::new(
        resident
            .begin_durable_private_execution(&durable, public)
            .unwrap(),
    );
    let producer_scope = producer_execution.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: producer_scope,
            ..public_context
        })
        .unwrap();
    let checked = check_fixture_cell(
        &mut resident,
        &recipe,
        include_str!("fixtures/activation-input-private-source.hs"),
        producer_execution.clone(),
        1,
    );
    assert_eq!(checked.checked.items.len(), 2);
    let private_declaration = checked.adopt_declaration(&mut resident);
    let private_owner = checked
        .checked
        .checked_item(0)
        .unwrap()
        .planned_declaration()
        .unwrap()
        .product()
        .owner()
        .clone();
    assert_eq!(private_owner.module, private_declaration);
    assert_eq!(
        before_selection
            .domain_for_owner(before_selection.current(), &private_owner)
            .unwrap(),
        before_selection.current()
    );

    let (bound, producer, reservation) = checked.compile_binding(&mut resident, 1);
    assert!(bound.is_empty());
    assert!(resident
        .current_decl_heads_in(producer_scope)
        .iter()
        .any(|(name, _)| name == "privateIncrement"));
    let fixture = InputFixture {
        root,
        session,
        recipe: recipe.clone(),
        producer,
    };
    let reservation_hole = fixture.start(&mut resident);
    let installed = resident
        .public_visibility_snapshot_in(producer_scope)
        .unwrap();
    assert_ne!(installed.source_instances, before.source_instances);
    assert!(installed
        .source_instances
        .iter()
        .any(|key| key.binder.binder.module == private_declaration));
    let (submission, activation) = fixture.deliver(&mut resident, reservation_hole, 1);
    let site = parked_site(&mut resident, &activation);
    let realm = resident.parked_realm(&activation).unwrap();
    let captured = resident
        .capture_activation_input(&activation, realm, site)
        .unwrap();
    resident.set_run_context(public_context).unwrap();
    let activation_execution = Arc::new(
        resident
            .begin_durable_private_execution(&durable, public)
            .unwrap(),
    );
    let activation_scope = activation_execution.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: activation_scope,
            ..public_context
        })
        .unwrap();
    let (input, interface) = issued_captured_input(&mut resident, captured, recipe.clone());
    let mounted = mount_original(&mut resident, input, interface);
    let published_binding = mounted.binding();
    assert!(!before_binding_ids.contains(&published_binding));
    let view = resident
        .compile_view_for_execution(&activation_execution)
        .unwrap()
        .with_scoped_injection();
    let preview = resident.admit_activation_preview(mounted, view).unwrap();
    let base = resident
        .snapshot_host_binding_publication(&activation_execution, &preview)
        .unwrap();
    assert!(
        base.source_keys.is_empty(),
        "binding reachability retains the private source without promoting its selection"
    );
    assert_eq!(
        resident
            .publish_staged_public_manifest(base.stage().unwrap(), &PublicationDecision::new())
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let published = resident.public_visibility_snapshot_in(public).unwrap();
    assert_eq!(published.source_instances, before.source_instances);
    let mut expected_binding_ids = before_binding_ids;
    expected_binding_ids.push(published_binding);
    expected_binding_ids.sort_by_key(|id| id.raw());
    assert_eq!(
        resident
            .state
            .bindings()
            .scope_reachable_binding_ids(resident.state.scope_tree(), public),
        expected_binding_ids
    );
    let mut expected_bindings = before.bindings.clone();
    expected_bindings.push(("sessionInput".into(), published_binding));
    expected_bindings.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(published.bindings, expected_bindings);
    assert_eq!(published.epoch, before.epoch + 1);
    assert_eq!(resident.state.bindings().tip_id(public), before_binding_tip);
    let selection = resident
        .state
        .bindings()
        .source_domain_selection_in(resident.state.scope_tree(), public)
        .unwrap();
    assert_eq!(selection.current(), before_selection.current());
    let selected_sources =
        |selection: &tidepool_codegen::prepared_program::SourceDomainSelection| {
            selection
                .inherited()
                .iter()
                .map(|(binder, lease)| {
                    (
                        binder.clone(),
                        tidepool_codegen::binding_table::SourceLeaseKey::of(lease),
                        lease.handle(),
                    )
                })
                .collect::<Vec<_>>()
        };
    assert_eq!(
        selected_sources(&selection),
        selected_sources(&before_selection)
    );
    assert_eq!(
        selection
            .domain_for_owner(selection.current(), &private_owner)
            .unwrap(),
        before_selection
            .domain_for_owner(before_selection.current(), &private_owner)
            .unwrap()
    );

    assert_eq!(published.declaration_tip, before.declaration_tip);
    assert!(!resident
        .current_decl_heads_in(public)
        .iter()
        .any(|(name, _)| name == "privateIncrement"));
    assert!(matches!(
        fixture.resume_activation(&mut resident, activation),
        ResidentOutcome::Completed { .. }
    ));
    assert!(matches!(
        settle_request_submission(&mut resident, submission),
        ResidentOutcome::Completed { .. }
    ));
    resident.set_run_context(public_context).unwrap();
    drop(preview);
    drop(reservation);
    drop(checked);
    let InputFixture {
        root: _source_root,
        producer,
        recipe: producer_recipe,
        ..
    } = fixture;
    drop(producer);
    drop(producer_recipe);
    drop(producer_execution);
    drop(activation_execution);
    resident.retire_scope(producer_scope);
    resident.retire_scope(activation_scope);
    assert!(resident.parked_holes().is_empty());
    assert_eq!(resident.outstanding_custody(), 0);
    let prepared = resident.state.prepared_mut().unwrap();
    let collections = prepared.heap_stats().gc_count;
    prepared.quiesce_and_collect_now().unwrap();
    assert_eq!(prepared.heap_stats().gc_count, collections + 1);
    let read = Arc::new(
        resident
            .begin_durable_private_execution(&durable, public)
            .unwrap(),
    );
    let read_scope = read.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: read_scope,
            ..public_context
        })
        .unwrap();
    let (bound, code, item) = compile_checked_binding(
        &mut resident,
        &recipe,
        "privateSourceResult <- pure (sessionInput (1 :: Int))",
        read.clone(),
    );
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            resident.run_bind_with_sites(
                "privateSourceResult",
                code.code(),
                &bound[0],
                item.generation(),
                settlement,
            )
        })
        .unwrap()
    else {
        panic!("published original closure must complete after collection");
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
    resident.set_run_context(public_context).unwrap();
    drop(item);
    drop(code);
    drop(read);
    resident.retire_scope(read_scope);
    assert_eq!(
        resident.public_visibility_snapshot_in(public).unwrap(),
        published
    );
}

fn assert_unpublished_input(
    resident: &mut TestSession,
    scope: ScopeId,
    visibility: &crate::session::PublicVisibilitySnapshot,
    handles_with_input: usize,
    persistent_roots: usize,
) {
    assert_eq!(resident.value_handle_count(), handles_with_input - 1);
    assert_eq!(resident.outstanding_custody(), 0);
    assert_eq!(
        resident.persistent_roots_count(),
        persistent_roots - 1,
        "refusal releases the affine input's owned root without publishing a binding"
    );
    assert_eq!(
        resident.public_visibility_snapshot_in(scope).as_ref(),
        Some(visibility)
    );
    assert!(!resident
        .binding_names_in(scope)
        .iter()
        .any(|name| name == "sessionInput"));
}

#[test]
fn activation_input_staging_does_not_publish_a_reserved_interface() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-function.hs"),
        false,
        SessionId(1737),
    );
    let mut resident = fixture.fresh();
    let (owner, interface) = parked_input_owner(&fixture, &mut resident);
    let certificate = interface.value_interface_certificate();
    let view = resident.compile_view_in(ScopeId::ROOT).unwrap();
    let path = view
        .session_root()
        .join(certificate.owner().relative_hi_path());
    let visibility = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let handles = resident.value_handle_count();
    let roots = resident.persistent_roots_count();
    let staged = resident
        .state
        .stage_checked_value_interface(certificate.clone())
        .unwrap();
    resident
        .state
        .validate_staged_value_interface(&staged)
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        certificate.bytes_owned().as_ref()
    );
    assert_eq!(
        resident.compile_view_in(ScopeId::ROOT).as_ref(),
        Some(&view)
    );
    assert_eq!(
        resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .as_ref(),
        Some(&visibility)
    );
    assert!(resident
        .state
        .retained_checked_value_artifact(certificate.owner())
        .is_none());
    assert_eq!(resident.value_handle_count(), handles);
    assert_eq!(resident.persistent_roots_count(), roots);
    let foreign_root = tempfile::tempdir().unwrap();
    let foreign = InputFixture::fresh_in(SessionId(1738), &foreign_root, &fixture.recipe);
    assert!(matches!(
        foreign.state.validate_staged_value_interface(&staged),
        Err(SessionError::StaleStagedDeclaration)
    ));
    drop(staged);
    assert!(
        !path.exists(),
        "dropping a stage cleans only its uncommitted creation"
    );
    mount_original(&mut resident, owner, interface);
    assert!(path.exists());
}

#[test]
fn activation_input_interface_staging_io_failure_precedes_root_consumption() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-function.hs"),
        false,
        SessionId(1731),
    );
    #[derive(Clone, Copy)]
    enum ExistingFile {
        Directory,
        Conflict,
        Identical,
    }
    for existing in [
        ExistingFile::Directory,
        ExistingFile::Conflict,
        ExistingFile::Identical,
    ] {
        let mut resident = fixture.fresh();
        let (owner, interface) = parked_input_owner(&fixture, &mut resident);
        let reservation = owner.reservation().clone();
        let certificate = interface.value_interface_certificate();
        let path = resident
            .compile_view_in(ScopeId::ROOT)
            .unwrap()
            .session_root()
            .join(certificate.owner().relative_hi_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        match existing {
            ExistingFile::Directory => std::fs::create_dir(&path).unwrap(),
            ExistingFile::Conflict => {
                std::fs::write(&path, b"unrelated immutable interface").unwrap()
            }
            ExistingFile::Identical => std::fs::write(&path, certificate.bytes_owned()).unwrap(),
        }
        let visibility = resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let handles = resident.value_handle_count();
        let roots = resident.persistent_roots_count();
        let requests = tidepool_toolchain::artifacts::host_binding_interface_request_count();
        match existing {
            ExistingFile::Identical => {
                let mounted = mount_original(&mut resident, owner, interface);
                drop(mounted);
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    certificate.bytes_owned().as_ref()
                );
            }
            ExistingFile::Directory | ExistingFile::Conflict => {
                let error = tidepool_testing::with_settlement(|settlement| {
                    resident.mount_activation_input(owner, interface.clone(), settlement)
                })
                .err()
                .expect("real filesystem failure must refuse before consuming input");
                assert!(
                    matches!(error, ResidentError::Session(SessionError::Io(_))),
                    "{error:?}"
                );
                assert_unpublished_input(&mut resident, ScopeId::ROOT, &visibility, handles, roots);
                assert!(
                    resident
                        .state
                        .validate_binding_interface(&reservation, &interface)
                        .is_ok(),
                    "staging failure cannot consume the reservation"
                );
                assert!(resident
                    .state
                    .retained_checked_value_artifact(certificate.owner())
                    .is_none());
                match existing {
                    ExistingFile::Directory => assert!(path.is_dir()),
                    ExistingFile::Conflict => assert_eq!(
                        std::fs::read(&path).unwrap(),
                        b"unrelated immutable interface"
                    ),
                    ExistingFile::Identical => unreachable!(),
                }
            }
        }
        assert_eq!(
            tidepool_toolchain::artifacts::host_binding_interface_request_count(),
            requests,
            "mount cannot retry interface issuance or rebuild a consumed root"
        );
        drop(resident);
        if path.is_dir() {
            std::fs::remove_dir(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
    }
}

#[test]
fn activation_input_mount_observes_invocation_and_resource_cancellation_before_transfer() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-function.hs"),
        false,
        SessionId(1732),
    );
    for invocation in [true, false] {
        let mut resident = fixture.fresh();
        let (owner, interface) = parked_input_owner(&fixture, &mut resident);
        let reservation = owner.reservation().clone();
        let certificate = interface.value_interface_certificate();
        let path = resident
            .compile_view_in(ScopeId::ROOT)
            .unwrap()
            .session_root()
            .join(certificate.owner().relative_hi_path());
        let visibility = resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let handles = resident.value_handle_count();
        let roots = resident.persistent_roots_count();
        let error = if invocation {
            resident
                .with_invocation_cancel(
                    Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    |resident| {
                        tidepool_testing::with_settlement(|settlement| {
                            resident.mount_activation_input(owner, interface.clone(), settlement)
                        })
                    },
                )
                .err()
        } else {
            resident
                .state
                .require_prepared()
                .unwrap()
                .cancel_handle(RealmId::ROOT)
                .cancel();
            tidepool_testing::with_settlement(|settlement| {
                resident.mount_activation_input(owner, interface.clone(), settlement)
            })
            .err()
        }
        .expect("observed cancellation refuses before affine root transfer");
        assert!(
            matches!(
                error,
                ResidentError::Prepared(PreparedRuntimeError::Cancelled)
            ),
            "{error:?}"
        );
        assert_unpublished_input(&mut resident, ScopeId::ROOT, &visibility, handles, roots);
        assert!(resident
            .state
            .validate_binding_interface(&reservation, &interface)
            .is_ok());
        assert!(resident
            .state
            .retained_checked_value_artifact(certificate.owner())
            .is_none());
        assert!(
            !path.exists(),
            "uncommitted interface staging removes only its own file"
        );
    }
}

#[test]
fn activation_input_mount_fences_owner_epoch_visibility_and_consumed_reservations() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-function.hs"),
        false,
        SessionId(1733),
    );
    enum Fence {
        OwnerEpoch,
        Visibility,
        Consumed,
        ForeignSession,
        Principal,
    }
    for fence in [
        Fence::OwnerEpoch,
        Fence::Visibility,
        Fence::Consumed,
        Fence::ForeignSession,
        Fence::Principal,
    ] {
        let mut resident = fixture.fresh();
        let (owner, interface) = parked_input_owner(&fixture, &mut resident);
        let reservation = owner.reservation().clone();
        match fence {
            Fence::OwnerEpoch => {
                let next = resident
                    .state
                    .prepare_execution_admission_epoch_advance()
                    .unwrap();
                resident
                    .state
                    .invalidate_execution_admissions_after_owner_transfer(next);
            }
            Fence::Visibility => resident.advance_public_visibility(ScopeId::ROOT),
            Fence::Consumed => resident
                .state
                .consume_binding_interface(&reservation, &interface)
                .unwrap(),
            Fence::ForeignSession => {
                let destination_root = tempfile::tempdir().unwrap();
                let mut destination =
                    InputFixture::fresh_in(SessionId(1734), &destination_root, &fixture.recipe);
                let visibility = destination
                    .public_visibility_snapshot_in(ScopeId::ROOT)
                    .unwrap();
                let roots = destination.persistent_roots_count();
                let handles = resident.value_handle_count();
                assert!(matches!(
                    tidepool_testing::with_settlement(|settlement| destination
                        .mount_activation_input(owner, interface, settlement)),
                    Err(ResidentError::ForeignCustody)
                ));
                assert_eq!(resident.value_handle_count(), handles - 1);
                assert_eq!(resident.outstanding_custody(), 0);
                assert_eq!(destination.persistent_roots_count(), roots);
                assert_eq!(
                    destination
                        .public_visibility_snapshot_in(ScopeId::ROOT)
                        .unwrap(),
                    visibility
                );
                continue;
            }
            Fence::Principal => {
                let mut context = resident.run_context();
                context.principal.incarnation += 1;
                resident.set_run_context(context).unwrap();
            }
        }
        let visibility = resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let handles = resident.value_handle_count();
        let roots = resident.persistent_roots_count();
        let error = tidepool_testing::with_settlement(|settlement| {
            resident.mount_activation_input(owner, interface, settlement)
        })
        .err()
        .expect("stale or consumed admission must not transfer the original root");
        assert!(
            matches!(
                error,
                ResidentError::Session(SessionError::StaleStagedDeclaration)
                    | ResidentError::InvalidActivationInput { .. }
            ),
            "{error:?}"
        );
        assert_unpublished_input(&mut resident, ScopeId::ROOT, &visibility, handles, roots);
    }
}

#[test]
fn activation_input_committed_binding_owns_interface_and_scope_releases_root_once() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-function.hs"),
        false,
        SessionId(1735),
    );
    let mut resident = fixture.fresh();
    let scope = resident.mint_detached_scope(ScopeId::ROOT).unwrap();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: scope,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let (owner, interface) = parked_input_owner(&fixture, &mut resident);
    let roots = resident.persistent_roots_count() - 1;
    let certificate = interface.value_interface_certificate();
    let certificate_lease = Arc::downgrade(&certificate);
    let prototype_lease = Arc::downgrade(interface.prototype());
    let mounted = mount_original(&mut resident, owner, interface);
    let binding = mounted.binding();
    drop(mounted);
    drop(certificate);
    assert!(
        certificate_lease.upgrade().is_some(),
        "binding owns its checked interface lease"
    );
    assert!(
        prototype_lease.upgrade().is_some(),
        "binding retains its original type/context prototype"
    );
    assert_eq!(resident.outstanding_custody(), 0);
    assert_eq!(resident.persistent_roots_count(), roots + 1);
    resident
        .state
        .require_prepared()
        .unwrap()
        .cancel_handle(RealmId::ROOT)
        .cancel();
    let parked = resident.parked_count();
    let retired = resident.retire_scope(scope);
    assert_eq!(retired.bindings_retired, 1);
    assert_eq!(retired.roots_released, 1);
    assert_eq!(resident.persistent_roots_count(), roots);
    assert_eq!(
        resident.parked_count(),
        parked,
        "scope cleanup leaves original parked callers owned by their realm"
    );
    assert!(resident.state.bindings().get(binding).is_none());
    assert!(resident.binding_provenance.get(&binding.raw()).is_none());
    assert_eq!(resident.retire_scope(scope), ScopeRetirement::default());
    assert!(certificate_lease.upgrade().is_none());
    assert!(prototype_lease.upgrade().is_none());
}

#[test]
fn checked_binding_survives_export_and_executes_in_later_cell() {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe {
        preamble: effects.preamble().to_owned(),
        row: effects.row().to_owned(),
        include: effects.include_paths().to_vec(),
    };
    let root = tempfile::tempdir().unwrap();
    let library = SessionLib::open(
        SessionId(1721),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(recipe.include.clone());
    let mut resident = TestSession::unbootstrapped(
        frunk::HNil,
        EmptyOutput,
        crate::DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    let execution = Arc::new(resident.begin_private_execution(ScopeId::ROOT).unwrap());
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let (bound, compiled, reservation) = compile_checked_binding(
        &mut resident,
        &recipe,
        "held <- pure (999999 :: Int)",
        execution.clone(),
    );
    let [binder] = bound.as_slice() else {
        panic!("held must retain one original checked binder");
    };
    let interface = compiled
        .certification
        .as_ref()
        .unwrap()
        .checked_execution()
        .unwrap()
        .value_interface_certificate()
        .unwrap();
    assert_eq!(
        interface.owner(),
        tidepool_repr::SessionModule::val(reservation.generation())
    );
    let outcome = tidepool_testing::with_settlement(|settlement| {
        resident.run_bind_with_sites(
            "held",
            compiled.code(),
            binder,
            reservation.generation(),
            settlement,
        )
    })
    .unwrap();
    assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
    publish_checked_fixture(&mut resident, &execution, 1);
    resident.set_run_context(SessionRunContext::ROOT).unwrap();
    let private_scope = execution.private_scope();
    drop(compiled);
    drop(reservation);
    drop(execution);
    resident.retire_scope(private_scope);
    assert!(Arc::ptr_eq(
        resident
            .state
            .retained_checked_value_artifact(interface.owner())
            .unwrap(),
        &interface
    ));
    let original = resident.current_binding_in(ScopeId::ROOT, "held").unwrap();
    assert_eq!(original.0, SessionVarId::from_extract(binder.var_id));
    let custody = resident.retain_binding_custody("held").unwrap().unwrap();
    let handles_before_export = resident.value_handle_count();
    let parcel = resident.export_custody(custody).unwrap();
    assert_eq!(resident.value_handle_count(), handles_before_export - 1);

    original_value_probe(&mut resident, &recipe, "held + 1", 1_000_000);
    assert_eq!(
        resident.current_binding_in(ScopeId::ROOT, "held"),
        Some(original)
    );
    assert!(Arc::ptr_eq(
        resident
            .state
            .retained_checked_value_artifact(interface.owner())
            .unwrap(),
        &interface
    ));
    drop(parcel);
    drop(interface);
    drop(resident);
}

fn compile_activation_preview(
    admission: Arc<RuntimeActivationPreviewAdmission>,
    template: &str,
    budget: u64,
    includes: &[std::path::PathBuf],
) -> Result<turn::ActivationPreviewCompilation, turn::ActivationRendererFailure> {
    let renderer = match admission
        .acquire_renderer(template, budget)
        .expect("admitted renderer slot")
    {
        RendererAccess::Ready(renderer) => renderer,
        RendererAccess::Produce(producer) => {
            producer.publish(tidepool_testing::with_settlement(|settlement| {
                turn::compile_activation_renderer(
                    &admission, template, budget, includes, settlement,
                )
            })?)
        }
        RendererAccess::Wait(_) => {
            panic!("synchronous semantic fixture has no concurrent producer")
        }
        RendererAccess::CloseUnconfirmed => {
            panic!("semantic fixture has no unconfirmed native close")
        }
    };
    turn::bind_activation_renderer(admission, &renderer).map_err(Into::into)
}

#[test]
fn owned_callable_result_authenticates_joint_capture_and_survives_source_retirement() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/owned-result-callable.hs"),
        false,
        SessionId(1750),
    );
    let mut source = InputFixture::fresh_with_receiver(
        SessionLib::open(
            fixture.session,
            fixture.root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(fixture.recipe.include.clone()),
        &fixture.recipe,
        include_str!("fixtures/owned-result-receiver.hs"),
        4,
    );
    let reservation = fixture.start(&mut source);
    let (submission, activation) = fixture.deliver(&mut source, reservation, 41);
    let site = parked_site(&mut source, &submission);
    let expected = source
        .request_result_type_witness(site, &submission)
        .unwrap();
    assert!(matches!(
        source.original_result_type_witness(site, &submission),
        Err(ResidentError::InvalidActivationInput { .. })
    ));
    assert!(matches!(
        source.original_result_type_witness(site ^ 1, &submission),
        Err(ResidentError::InvalidActivationInput { .. })
    ));
    let reply = source
        .retain_binding_custody("activationFunctionReply")
        .unwrap()
        .unwrap();
    let publication = suspended(
        tidepool_testing::with_settlement(|settlement| {
            source.resume_handle(activation, reply, settlement)
        })
        .unwrap(),
    );
    let baseline = source.outstanding_custody();
    let native_roots = source.value_handle_count();
    assert!(matches!(
        source.capture_result_publication(&publication, site ^ 1, RealmId::ROOT),
        Err(ResidentError::InvalidActivationInput { .. })
    ));
    assert!(matches!(
        source.capture_result_publication(&publication, site, RealmId::fresh()),
        Err(ResidentError::InvalidActivationInput { .. })
    ));
    let row = source
        .parked
        .iter()
        .position(|entry| entry.name == publication.cont_id())
        .unwrap();
    let genuine = source.parked[row].provenance.clone();
    let mut withdrawn = (*genuine).clone();
    withdrawn.authenticated_inputs.remove(&site);
    source.parked[row].provenance = Arc::new(withdrawn);
    assert!(matches!(
        source.capture_result_publication(&publication, site, RealmId::ROOT),
        Err(ResidentError::UnauthenticatedActivationInputWitness { .. })
    ));
    let mut missing = (*genuine).clone();
    missing.sites.remove(&site);
    source.parked[row].provenance = Arc::new(missing);
    assert!(matches!(
        source.capture_result_publication(&publication, site, RealmId::ROOT),
        Err(ResidentError::InvalidActivationInput { .. })
    ));
    source.parked[row].provenance = genuine;
    assert_eq!(
        source.outstanding_custody(),
        baseline,
        "refusals allocate no result root"
    );
    assert_eq!(source.value_handle_count(), native_roots);
    let captured = source
        .capture_result_publication(&publication, site, RealmId::ROOT)
        .unwrap();
    assert_eq!(&expected, captured.type_witness());
    assert_eq!(source.outstanding_custody(), baseline + 1);
    assert_eq!(source.value_handle_count(), native_roots + 1);
    let source_lease = source.lease_bindings(&[]);
    assert!(captured.belongs_to_bindings(&source_lease));
    let parcel = source.export_result_shared(&captured).unwrap();
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| {
            source.resume(publication, (), settlement)
        })
        .unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    assert!(matches!(
        settle_request_submission(&mut source, submission),
        ResidentOutcome::Completed { .. }
    ));
    drop(source_lease);
    let before_release = source.value_handle_count();
    drop(captured);
    source.settle_dropped_custody();
    assert_eq!(
        source.value_handle_count(),
        before_release - 1,
        "the capture root is physical: dropping custody releases its native root"
    );
    assert_eq!(
        source.outstanding_custody(),
        0,
        "source result root releases before machine retirement"
    );
    drop(source);
    let destination_root = tempfile::tempdir().unwrap();
    let mut destination = InputFixture::fresh_with_receiver(
        SessionLib::open(
            SessionId(1751),
            destination_root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(fixture.recipe.include.clone()),
        &fixture.recipe,
        include_str!("fixtures/owned-result-receiver.hs"),
        4,
    );
    let imported = destination.import_result(parcel).unwrap();
    let destination_lease = destination.lease_bindings(&[]);
    assert!(imported.belongs_to_bindings(&destination_lease));
    assert_eq!(&expected, imported.type_witness());
    destination
        .state
        .require_prepared()
        .unwrap()
        .quiesce_and_collect_now()
        .unwrap();
    let probe = destination
        .retain_binding_custody("ownedResultProbe")
        .unwrap()
        .unwrap();
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            destination.run_rooted_application(
                "forceRetiredCallableResult",
                &probe,
                imported.custody(),
                RealmId::ROOT,
                None,
                settlement,
            )
        })
        .unwrap()
    else {
        panic!("native callable result probe must complete")
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
    drop(probe);
    drop(imported);
    drop(destination_lease);
    destination.settle_dropped_custody();
    assert_eq!(destination.outstanding_custody(), 0);
}
