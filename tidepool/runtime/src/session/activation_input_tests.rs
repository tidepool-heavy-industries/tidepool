//! Real parked inputs through the distinct checked host mount and preview.

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
        include.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/actors"));
        let mut preamble = effects.preamble().to_owned();
        for import in [
            "Tidepool.Agent.Reply (Replies)",
            "Tidepool.Agent.Ref (AgentProtocol(..))",
            "qualified Tidepool.Agent.Ref as Ref",
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

    fn fresh(&self) -> TestSession {
        let library = SessionLib::open(
            self.session,
            self.root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(self.recipe.include.clone());
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
        let (bound, receiver, reservation) = compile_checked_binding(
            &mut resident,
            &self.recipe,
            include_str!("fixtures/activation-input-receiver.hs"),
            None,
        );
        assert_eq!(bound.len(), 2, "receiver and native Unit reply bindings");
        for name in ["activationReceiver", "activationUnitReply"] {
            assert_eq!(bound.iter().filter(|binder| binder.name == name).count(), 1);
        }
        assert!(
            receiver.asks.is_empty(),
            "pure receiver setup has no request sites"
        );
        let execution = receiver
            .certification
            .as_ref()
            .and_then(|certificate| certificate.checked_execution.as_ref())
            .expect("receiver setup has its checked native output proof");
        assert!(execution.matches_target(&receiver.prepared));
        let interface = execution
            .value_interface_certificate()
            .expect("receiver setup issued its exact value-interface certificate");
        assert!(interface.is_checked_output());
        assert_eq!(
            interface.owner(),
            tidepool_repr::SessionModule::val(reservation.generation())
        );
        let outcome = resident
            .run_projected_bind_with_sites(
                "nativeActivationReceiver",
                receiver.code(),
                &bound,
                reservation.generation(),
            )
            .expect("install the checked native protocol receiver and Unit reply");
        assert!(
            matches!(outcome, ResidentOutcome::BindingsCommitted { .. }),
            "projected native binding completion: {outcome:?}"
        );
        let retained = resident
            .state
            .retained_checked_value_artifact(interface.owner())
            .expect("receiver settlement retained the checked value-interface proof");
        assert!(Arc::ptr_eq(retained, &interface));
        for binder in &bound {
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
            resident.resume(hole.clone(), ()),
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
        resident
            .resume_handle(hole, reply)
            .expect("deliver native reply custody into the original request")
    }

    fn start(&self, resident: &mut TestSession) -> ResidentHole {
        suspended(
            resident
                .run_with_sites("originalActivationRequests", self.producer.code())
                .expect("execute authentic compiled request sites"),
        )
    }

    fn deliver(
        &self,
        resident: &mut TestSession,
        reservation: ResidentHole,
        request: i64,
    ) -> (ResidentHole, ResidentHole) {
        let submission = suspended(
            resident
                .resume(reservation, request)
                .expect("settle native request reservation"),
        );
        // SubmitRequestWith carries request id at field0 and protocol payload at field1.
        let payload = resident
            .live_payload_handle(submission.cont_id())
            .unwrap()
            .expect("original RunRequest remains a live native value");
        let receiver = resident
            .retain_binding_custody("activationReceiver")
            .unwrap()
            .expect("real compiled native receiver");
        let activation = suspended(
            resident
                .run_rooted_application(
                    "realRequestSession",
                    &receiver,
                    &payload,
                    RealmId::ROOT,
                    None,
                )
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

fn assert_startup_origin(label: &str, compiled: &CompiledTurn, requires_input: bool) {
    let proof = compiled
        .certification
        .as_ref()
        .and_then(|certification| certification.compile_input_identity.as_ref());
    let matches = compiled
        .certification
        .as_ref()
        .is_some_and(|certification| {
            proof.is_some_and(|proof| {
                proof.matches_bundle(
                    &compiled.prepared,
                    &certification.groups,
                    &certification.target_owners,
                    &certification.package_interfaces,
                    &compiled.table,
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
            "compile_input_identity_present": proof.is_some(),
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
    run_turn(TurnRequest {
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
        .parked(id)
        .unwrap()
        .1
        .site
}

fn suspended(outcome: ResidentOutcome) -> ResidentHole {
    match outcome {
        ResidentOutcome::Suspended { hole, .. } => hole,
        other => panic!("expected native request suspension, got {other:?}"),
    }
}

fn checked_input(
    resident: &mut TestSession,
    hole: &ResidentHole,
    site: u64,
    recipe: Arc<InputRecipe>,
) -> (RuntimeActivationInputAdmission, CompiledActivationInput) {
    let realm = resident.parked_realm(hole).expect("original parked realm");
    let input = resident
        .capture_activation_input(hole, realm, site)
        .expect("claim original input and authenticated canonical type");
    checked_captured_input(resident, input, recipe)
}

fn checked_captured_input(
    resident: &mut TestSession,
    input: RuntimeActivationInput,
    recipe: Arc<InputRecipe>,
) -> (RuntimeActivationInputAdmission, CompiledActivationInput) {
    let view = resident.compile_view_in(ScopeId::ROOT).unwrap();
    let imports = view.turn_imports(&SourceImports::new());
    let template = resident_cell_check_template(&recipe.preamble, &recipe.row, &imports);
    let mut owner = resident
        .admit_activation_input_in(
            ScopeId::ROOT,
            input,
            &recipe.preamble,
            &recipe.row,
            512,
            template,
            recipe.clone(),
            recipe.digest(),
            view.include_paths(&recipe.include),
            String::new(),
        )
        .expect("seal original input under the owning scope and source recipe");
    assert_eq!(
        owner.specification.injected_modules,
        owner.admission.view().injected_module_names()
    );
    let original_specification = owner.specification.clone();
    let before = resident.residency();
    let visibility = resident
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let mut injections = vec![{
        let mut injected = original_specification.injected_modules.clone();
        injected.push("Val.G999999".into());
        injected
    }];
    if original_specification.injected_modules.len() > 1 {
        injections.push(
            original_specification
                .injected_modules
                .iter()
                .rev()
                .cloned()
                .collect(),
        );
    }
    for injected_modules in injections {
        let mut changed = (*original_specification).clone();
        changed.injected_modules = injected_modules;
        owner.specification = Arc::new(changed);
        let failure = turn::check_activation_input(&owner)
            .expect_err("extra or reordered injection must be refused before compilation");
        assert!(matches!(
            failure.error,
            crate::CompileError::ExtractFailed(_)
        ));
        assert_eq!(resident.residency(), before);
        assert_eq!(
            resident
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            visibility
        );
    }
    owner.specification = original_specification;
    let checked = turn::check_activation_input(&owner).expect("check host input interface");
    let item = resident
        .admit_activation_input_item(&owner, &checked)
        .expect("reserve only the checked host input item");
    let compiled = turn::compile_activation_input(&owner, item)
        .expect("compile opaque host recipe and independently infer its input type");
    (owner, compiled)
}

fn preview_original(
    resident: &mut TestSession,
    owner: RuntimeActivationInputAdmission,
    compiled: CompiledActivationInput,
) {
    let mounted = resident
        .mount_activation_input(owner, compiled)
        .expect("mount the original value without executing placeholder source");
    assert!(resident
        .binding_names_in(ScopeId::ROOT)
        .iter()
        .any(|name| name == "sessionInput"));
    let ResidentOutcome::Completed { result, .. } = resident
        .run_activation_preview(mounted)
        .expect("dedicated preview evaluates the mounted original input")
    else {
        panic!("pure host preview must complete");
    };
    let result = result.to_json();
    let tuple = result.as_array().expect("preview text and truncation flag");
    assert_eq!(tuple.len(), 2);
    assert!(tuple[0].is_string());
    assert_eq!(tuple[1], false);
}

fn compile_checked_binding(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    source: &str,
    execution: Option<Arc<crate::session::PrivateExecutionAdmission>>,
) -> (
    Vec<BoundBinder>,
    CompiledTurn,
    Arc<crate::session::RuntimeCheckedItemAdmission>,
) {
    use crate::session::{CellCheckRequest, TemplateSelector};
    use tidepool_toolchain::checked_cell::CheckedCellSpecification;

    let scope = resident.run_context().lexical_scope;
    let view = resident
        .compile_view_in(scope)
        .unwrap()
        .with_scoped_injection();
    let imports = view.turn_imports(&SourceImports::new());
    let template = resident_cell_check_template(&recipe.preamble, &recipe.row, &imports);
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
    let admission = match execution {
        Some(execution) => resident.admit_cell_for_execution(
            execution,
            0,
            specification.clone(),
            specification.specification_digest(),
            recipe.digest(),
            view.include_paths(&recipe.include),
        ),
        None => resident.admit_cell_in(
            scope,
            0,
            specification.clone(),
            specification.specification_digest(),
            recipe.digest(),
            view.include_paths(&recipe.include),
        ),
    }
    .expect("admit checked fixture bindings with their selected interfaces");
    let view = admission.view();
    let includes = admission.include_paths().to_vec();
    let include = includes.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = view.injected_module_names();
    let checked = turn::check_cell_admitted(
        CellCheckRequest {
            exact_context: view.exact_compile_context(),
            session_id: Some(view.session()),
            cell_text: source,
            template: &template,
            include: &include,
            session_root: view.session_root(),
            inject_modules: &injected,
            compile_generation: admission.initial_value_generation().0,
            compile_view_evidence: "",
        },
        admission.clone(),
        &templates,
    )
    .expect("check fixture bindings through runtime admission");
    let item = checked.checked_item(0).unwrap();
    let prefix = resident
        .begin_checked_prefix(admission, item.clone())
        .unwrap();
    let reservation = resident.admit_checked_item(prefix, item).unwrap();
    let snapshot = reservation.snapshot();
    let view = snapshot.view();
    let injected = snapshot.compiler_prefix().injected_modules();
    let TurnResult::Bind {
        bound, compiled, ..
    } = turn::run_checked_item(
        TurnRequest {
            exact_context: view.exact_compile_context(),
            session_id: Some(view.session()),
            turn_text: source,
            templates: &templates,
            include: &include,
            session_root: view.session_root(),
            inject_modules: &injected,
            gen: reservation.generation().0,
            verdict: Some(checked.items[0].verdict.clone()),
            target: None,
            retained_imports: snapshot.admitted_retained_imports(),
        },
        reservation.clone(),
    )
    .expect("compile admitted fixture bindings")
    else {
        panic!("fixture setup or value probe must be a checked bind");
    };
    (bound, compiled, reservation)
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
        compile_checked_binding(resident, recipe, &source, Some(execution));
    assert_eq!(bound.len(), 1);
    let ResidentOutcome::Completed { result, .. } = resident
        .run_bind_with_sites(
            "originalInputProbe",
            compiled.code(),
            &bound[0],
            reservation.generation(),
        )
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
    use crate::session::{CellCheckRequest, TemplateSelector};
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
        .admit_cell_for_execution(
            execution.clone(),
            0,
            specification.clone(),
            specification.specification_digest(),
            fixture.recipe.digest(),
            view.include_paths(&fixture.recipe.include),
        )
        .unwrap();
    let view = admission.view();
    let include_paths = admission.include_paths().to_vec();
    let include = include_paths
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let injected = view.injected_module_names();
    let checked = turn::check_cell_admitted(
        CellCheckRequest {
            exact_context: view.exact_compile_context(),
            session_id: Some(view.session()),
            cell_text: source,
            template: &template,
            include: &include,
            session_root: view.session_root(),
            inject_modules: &injected,
            compile_generation: admission.initial_value_generation().0,
            compile_view_evidence: "",
        },
        admission.clone(),
        &templates,
    )
    .unwrap();
    let item = checked.checked_item(0).unwrap();
    let prefix = resident
        .begin_checked_prefix(admission, item.clone())
        .unwrap();
    let reservation = resident.admit_checked_item(prefix.clone(), item).unwrap();
    let snapshot = reservation.snapshot();
    let view = snapshot.view();
    let injected = snapshot.compiler_prefix().injected_modules();
    let TurnResult::Bind {
        bound, compiled, ..
    } = turn::run_checked_item(
        TurnRequest {
            exact_context: view.exact_compile_context(),
            session_id: Some(view.session()),
            turn_text: source,
            templates: &templates,
            include: &include,
            session_root: view.session_root(),
            inject_modules: &injected,
            gen: reservation.generation().0,
            verdict: Some(checked.items[0].verdict.clone()),
            target: None,
            retained_imports: snapshot.admitted_retained_imports(),
        },
        reservation.clone(),
    )
    .unwrap()
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
    let error = resident
        .run_bind_with_sites(
            "changedCheckedSiteMap",
            changed,
            &bound[0],
            reservation.generation(),
        )
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
        let (owner, compiled) = checked_input(&mut resident, &hole, site, fixture.recipe.clone());
        let before = resident
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let prefix = compiled.admission.prefix().snapshot();
        let residency = resident.residency();
        let mut changed = compiled.compiled.code();
        let mut sites = changed.sites.to_vec();
        sites.push(
            fixture
                .producer
                .asks
                .iter()
                .find(|candidate| candidate.site == site)
                .unwrap()
                .clone(),
        );
        changed.sites = Cow::Owned(sites);
        let error = resident
            .run_with_sites("changedHostSiteMap", changed)
            .err()
            .expect("sealed host site map must reject tampering");
        assert!(
            matches!(error, ResidentError::Session(SessionError::Compile(_))),
            "{error:?}"
        );
        assert_eq!(resident.residency(), residency);
        assert!(Arc::ptr_eq(
            &prefix,
            &compiled.admission.prefix().snapshot()
        ));
        assert_eq!(
            resident
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            before
        );
        assert!(matches!(
            resident.run_bind_with_sites(
                "authoredRouteMustNotExecutePlaceholder",
                compiled.compiled.code(),
                &compiled.binder,
                compiled.admission.generation(),
            ),
            Err(ResidentError::UnsupportedCheckedTurn)
        ));
        assert!(Arc::ptr_eq(
            &prefix,
            &compiled.admission.prefix().snapshot()
        ));
        assert_eq!(
            resident
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            before
        );
        preview_original(&mut resident, owner, compiled);
        original_value_probe(
            &mut resident,
            &fixture.recipe,
            "sessionInput (41 :: Int)",
            value,
        );
        previous_hole = Some(hole.clone());
        let outcome = fixture.resume_activation(&mut resident, hole);
        assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
        let caller = resident
            .resume(submission, ())
            .expect("settle original native submission");
        if request == 1 {
            reservation = suspended(caller);
        } else {
            assert!(matches!(caller, ResidentOutcome::Completed { .. }));
            assert!(resident.parked_holes().is_empty());
            break;
        }
    }

    // Existing native exports are warmed by the original certified program;
    // running its unchanged graph without the compiler seal does not retain
    // authenticated type evidence for a subsequent host capture.
    let mut unsealed = fixture.fresh();
    let warm = fixture.start(&mut unsealed);
    let mut code = fixture.producer.code();
    code.certification = Cow::Owned(None);
    let reservation = suspended(
        unsealed
            .run_with_sites("unsealedOriginalSiteGraph", code)
            .expect("generic graph execution can retain unsealed site observations"),
    );
    let (submission, hole) = fixture.deliver(&mut unsealed, reservation, 1);
    let site = parked_site(&mut unsealed, &hole);
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
    let mut owned_holes = [warm, submission, hole]
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
            unsealed.abort(&id, "release fixture-owned request".into()),
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

#[test]
fn activation_opaque_input_native_owner_survives_same_spelling_source_shadow() {
    let fixture = InputFixture::compile(
        include_str!("fixtures/activation-input-opaque.hs"),
        true,
        SessionId(1711),
    );
    let replacement = fixture
        .recipe
        .preamble
        .replace(
            "import ActivationInputOriginal (Input)",
            "import ActivationInputReplacement (Input)",
        )
        .replace(
            "import qualified ActivationInputOriginal as Original",
            "import qualified ActivationInputReplacement as Original",
        )
        .replace(
            "import qualified ActivationInputOriginal\n",
            "import qualified ActivationInputReplacement as ActivationInputOriginal\n",
        );
    let shadow = Arc::new(InputRecipe {
        preamble: replacement,
        row: fixture.recipe.row.clone(),
        include: fixture.recipe.include.clone(),
    });
    let mut refused = fixture.fresh();
    let reservation = fixture.start(&mut refused);
    let (submission, hole) = fixture.deliver(&mut refused, reservation, 1);
    let site = parked_site(&mut refused, &hole);
    let before = refused
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let (owner, compiled) = checked_input(&mut refused, &hole, site, shadow);
    preview_original(&mut refused, owner, compiled);
    assert_ne!(
        refused
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap(),
        before
    );
    assert!(refused
        .binding_names_in(ScopeId::ROOT)
        .iter()
        .any(|name| name == "sessionInput"));
    assert_eq!(refused.outstanding_custody(), 0);
    assert!(matches!(
        fixture.resume_activation(&mut refused, hole),
        ResidentOutcome::Completed { .. }
    ));
    assert!(matches!(
        refused.resume(submission, ()).unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    drop(refused);

    // Reuse the immutable original fixture, with fresh mutable native state.
    let mut accepted = fixture.fresh();
    let reservation = fixture.start(&mut accepted);
    let (submission, hole) = fixture.deliver(&mut accepted, reservation, 1);
    let site = parked_site(&mut accepted, &hole);
    let (owner, compiled) = checked_input(&mut accepted, &hole, site, fixture.recipe.clone());
    preview_original(&mut accepted, owner, compiled);
    original_value_probe(
        &mut accepted,
        &fixture.recipe,
        "Original.project sessionInput",
        42,
    );
    assert!(matches!(
        fixture.resume_activation(&mut accepted, hole),
        ResidentOutcome::Completed { .. }
    ));
    assert!(matches!(
        accepted.resume(submission, ()).unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    assert!(accepted.parked_holes().is_empty());
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
    let original = fixture
        .producer
        .certification
        .as_ref()
        .unwrap()
        .artifact_view
        .descriptors()
        .into_iter()
        .find(|descriptor| descriptor.owner.module == "ActivationInputOriginal")
        .expect("compiler retained the actual original private home interface");
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
    assert!(retained
        .declarations()
        .artifact_view()
        .descriptors()
        .contains(&original));
    assert!(
        retained.declarations().lexical_graph().is_empty(),
        "type custody grants no source names"
    );
    drop(retained);
    assert!(inventory.node_count() > 0);

    let (owner, compiled) = checked_captured_input(&mut resident, input, recipe);
    assert!(
        compiled
            .compiled
            .certification
            .as_ref()
            .expect("native generation sealed products")
            .artifact_view
            .descriptors()
            .contains(&original),
        "native generation carries its original type-only dependency"
    );
    preview_original(&mut resident, owner, compiled);
    assert!(inventory.node_count() > 0);
    drop(evidence);
    drop(resident);
    assert_eq!(
        inventory.node_count(),
        0,
        "the final native/value/request reader releases the original graph"
    );
}
