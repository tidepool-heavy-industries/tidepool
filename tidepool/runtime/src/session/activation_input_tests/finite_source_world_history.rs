//! Finite source publication and later cells use the actual B source owner.
use super::*;
use std::collections::BTreeMap;
use tidepool_codegen::prepared_program::{
    NativeBindingKey, NativeInstanceLease, SourceInstanceDomain,
};
use tidepool_repr::execution_schema::{Group, HeapRhs, UpdatePolicy, ValueId};

type CafKey = (NativeBindingKey, ValueId);

#[derive(Debug)]
struct HeldPackageCaf {
    domain: SourceInstanceDomain,
    lease: NativeInstanceLease,
    pointer: usize,
    evaluated: bool,
}

fn observed_package_cafs(
    resident: &mut TestSession,
    scope: ScopeId,
) -> BTreeMap<CafKey, HeldPackageCaf> {
    let selection = resident
        .state
        .bindings()
        .native_domain_selection_in(resident.state.scope_tree(), scope)
        .expect("the actual source owner issues its selected native domain");
    let domain = selection.current();
    let leases = selection
        .roots()
        .iter()
        .filter(|(key, _)| key.domain == domain)
        .map(|(_, lease)| lease.clone())
        .collect::<Vec<_>>();
    let mut observations = BTreeMap::<CafKey, HeldPackageCaf>::new();
    for lease in leases {
        let NativeInstanceLease::Package(package) = &lease else {
            continue;
        };
        let values = package
            .image()
            .group()
            .definitions()
            .bindings()
            .iter()
            .flat_map(|group| match group {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops.as_slice(),
            })
            .filter(|top| {
                matches!(
                    top.binding.rhs,
                    HeapRhs::Thunk {
                        update: UpdatePolicy::Memoize,
                        ..
                    }
                )
            })
            .map(|top| top.binding.id)
            .collect::<Vec<_>>();
        for value in values {
            let key = (lease.binding_key(), value);
            if let Some(previous) = observations.get(&key) {
                assert_eq!(previous.lease.instance(), lease.instance(),
                    "this actual scope must not conflate two physical instances for one package binding");
                continue;
            }
            let observed = crate::session::prepared::tests::observe_package_instance(
                resident
                    .state
                    .require_prepared()
                    .expect("one actual physical engine"),
                &lease,
                value,
            )
            .expect("the actual package image/program/value/root pairing remains live");
            observations.insert(
                key,
                HeldPackageCaf {
                    domain,
                    lease: lease.clone(),
                    pointer: observed.pointer,
                    evaluated: observed.evaluated,
                },
            );
        }
    }
    observations
}

fn assert_same_package_instance(previous: &HeldPackageCaf, current: &HeldPackageCaf) {
    assert_eq!(previous.domain, current.domain);
    assert_eq!(previous.lease.binding_key(), current.lease.binding_key());
    assert_eq!(
        previous.lease.instance(),
        current.lease.instance(),
        "repeated actual-domain demand must preserve mutable package state"
    );
    let (NativeInstanceLease::Package(a), NativeInstanceLease::Package(b)) =
        (&previous.lease, &current.lease)
    else {
        unreachable!()
    };
    assert!(
        Arc::ptr_eq(a.image().image(), b.image().image()),
        "the actual root retains its same immutable package code"
    );
}

fn assert_unchanged_public_caf<'a>(
    current: &'a BTreeMap<CafKey, HeldPackageCaf>,
    key: CafKey,
    previous: &HeldPackageCaf,
) -> &'a HeldPackageCaf {
    let NativeInstanceLease::Package(expected) = &previous.lease else {
        unreachable!()
    };
    // This fixture never changes the package selection. An old held root must
    // not conceal an additional installation of that same immutable image.
    let matching = current
        .iter()
        .filter(|((_, value), actual)| {
            let NativeInstanceLease::Package(actual) = &actual.lease else {
                return false;
            };
            *value == key.1 && Arc::ptr_eq(expected.image().image(), actual.image().image())
        })
        .collect::<Vec<_>>();
    let [(actual_key, actual)] = matching.as_slice() else {
        panic!("unchanged public B must select one physical CAF for this image: {matching:?}");
    };
    assert_eq!(
        **actual_key, key,
        "the SAME issued package binding must survive each B item"
    );
    assert_same_package_instance(previous, actual);
    actual
}

#[test]
#[ignore = "requires the rooted runtime_package_source_history_test resources"]
fn finite_source_original_feeds_public_world_cells_after_prior_cutoff() {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let root = tempfile::tempdir().unwrap();
    let authored = root.path().join("authored");
    std::fs::create_dir(&authored).unwrap();
    std::fs::write(
        authored.join("PackageCafIsolation.hs"),
        include_str!("../../../tests/fixtures/PackageCafIsolation.hs"),
    )
    .unwrap();
    let mut includes = effects.include_paths().to_vec();
    includes.push(authored);
    let recipe = InputRecipe::capture(
        insert_preamble_imports(
            effects.preamble(),
            "qualified PackageCafIsolation as PackageProbe",
        ),
        effects.row().into(),
        includes,
    );
    let library = SessionLib::open(
        SessionId(1797),
        root.path().join("session"),
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
    let initial_context = resident.public_context();
    resident
        .set_actor_execution(
            initial_context,
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        )
        .unwrap();
    let templates = [turn::TurnTemplate {
        kind: crate::session::TemplateSelector::Expr,
        source: crate::session::assemble_expression_module(
            &recipe.preamble,
            "__result",
            &recipe.row,
            "{{TURN}}",
            crate::session::ExpressionLift::Effectful,
        ),
    }];
    let include = recipe
        .include
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let finite_root = root.path().join("finite");
    std::fs::create_dir(&finite_root).unwrap();
    let entry = tidepool_testing::with_settlement(|settlement| {
        run_turn(
            TurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: "pure (if PackageProbe.packageProbe 0 > 0 then (42 :: Int) else 0)",
                templates: &templates,
                include: &include,
                session_root: &finite_root,
                inject_modules: &[],
                gen: 1,
                verdict: None,
                target: None,
                retained_imports: &[],
            },
            settlement,
        )
    })
    .expect("compile the real finite native source through the paired output owner");
    let prepared = crate::session::PreparedSourceEntry::prepare(
        Arc::new(compiled(entry)),
        Arc::new(tidepool_codegen::prepared_program::ImageRegistry::new()),
    )
    .expect("close the actual finite source entry without ambient bindings");
    let selected = prepared
        .published_source_original_selection(
            prepared
                .ordinary_original_compile_input()
                .original_input_identity(),
            &tidepool_toolchain::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "PackageCafIsolation".into(),
            },
        )
        .expect("the SAME prepared output issues its source-original selection");

    let source_a = resident.public_source.clone();
    let unpublished_b = resident.mint_unpublished_notebook_scope(None).unwrap();
    let source_b = unpublished_b.lease().clone();
    assert_ne!(source_a.scope(), source_b.scope());
    let capsule_a = prepare_package_source(&mut resident, &prepared, &source_a);
    let capsule_b = prepare_package_source(&mut resident, &prepared, &source_b);
    let installation_a = capsule_a.installation_scope().clone();
    let installation_b = capsule_b.installation_scope().clone();
    assert_ne!(installation_a.scope(), installation_b.scope());
    // Read both roots before execution can collect or update either closure.
    let initial_a = observed_package_cafs(&mut resident, installation_a.scope());
    let initial_b = observed_package_cafs(&mut resident, installation_b.scope());
    assert!(
        !initial_a.is_empty(),
        "the real selected package groups must contain a memoizing CAF"
    );
    assert_eq!(
        initial_a.keys().collect::<Vec<_>>(),
        initial_b.keys().collect::<Vec<_>>()
    );
    for (key, a) in &initial_a {
        let b = &initial_b[key];
        assert_ne!(a.domain, b.domain);
        assert_ne!(a.lease.instance(), b.lease.instance());
        assert_ne!(
            a.pointer, b.pointer,
            "fresh A/B CAFs must be distinct heap objects: {key:?}"
        );
        let (NativeInstanceLease::Package(a), NativeInstanceLease::Package(b)) =
            (&a.lease, &b.lease)
        else {
            unreachable!()
        };
        assert!(
            Arc::ptr_eq(a.image().image(), b.image().image()),
            "fresh installation shares immutable package code only"
        );
    }
    run_package_source(&mut resident, capsule_a, &source_a);
    let forced_a = observed_package_cafs(&mut resident, installation_a.scope());
    let untouched_b = observed_package_cafs(&mut resident, installation_b.scope());
    let forced_key = initial_a.iter().find_map(|(key, initial)| {
        (!initial.evaluated && !initial_b[key].evaluated && forced_a[key].evaluated
            && !untouched_b[key].evaluated).then_some(*key)
    }).unwrap_or_else(|| panic!(
        "real A execution must force a selected CAF while B stays unforced: A={initial_a:?} after={forced_a:?} B={untouched_b:?}"
    ));
    assert_same_package_instance(&initial_a[&forced_key], &forced_a[&forced_key]);
    assert_same_package_instance(&initial_b[&forced_key], &untouched_b[&forced_key]);
    run_package_source(&mut resident, capsule_b, &source_b);
    let forced_b = observed_package_cafs(&mut resident, installation_b.scope());
    assert_same_package_instance(&initial_b[&forced_key], &forced_b[&forced_key]);
    assert!(
        forced_b[&forced_key].evaluated,
        "B forces its own actual CAF"
    );

    // Public B is its actual issued lease, not installer I_B or a named substitute.
    resident.public_source = source_b.clone();
    resident.set_run_context(resident.public_context()).unwrap();
    let public_b = source_b.scope();
    let staged = source_action(&mut resident, |resident, _| {
        resident.stage_published_source_originals_in(public_b, selected)
    })
    .unwrap();
    source_action(&mut resident, |resident, _| {
        resident.publish_source_originals(staged)
    })
    .unwrap();
    resident.bootstrap_world_in(&recipe, &source_b);
    public_package_cell(
        &mut resident,
        &recipe,
        "packageFirst",
        "if PackageProbe.packageProbe 0 > 0 then (42 :: Int) else 0",
        42,
    );
    let public_first = observed_package_cafs(&mut resident, public_b);
    let first_caf = public_first
        .get(&forced_key)
        .expect("the first B cell demands the SAME issued package CAF binding");
    assert!(first_caf.evaluated);
    assert_ne!(first_caf.domain, forced_b[&forced_key].domain);
    assert_ne!(
        first_caf.lease.instance(),
        forced_b[&forced_key].lease.instance(),
        "public B and private installer I_B are separate physical domains"
    );
    public_package_cell(
        &mut resident,
        &recipe,
        "packageSecond",
        "P.fst packageFirst + if PackageProbe.packageProbe 1 > 0 && PackageProbe.packageSibling '\\NUL' > 0 then 1 else 0",
        43,
    );
    let public_second = observed_package_cafs(&mut resident, public_b);
    assert!(assert_unchanged_public_caf(&public_second, forced_key, first_caf).evaluated);
    let cutoff = source_a.source_admission().fence_if_idle().unwrap();
    assert!(cutoff
        .retire(
            crate::session::NotebookGenerationId::INITIAL
                .next()
                .unwrap()
        )
        .is_ok());
    assert!(matches!(
        source_a.admit_source(),
        Err(crate::session::SourceAdmissionError::Closed { .. })
    ));
    public_package_cell(
        &mut resident,
        &recipe,
        "packageAfterCutoff",
        "P.fst packageSecond + if PackageProbe.packageProbe 2 > 0 then 1 else 0",
        44,
    );
    let public_after_cutoff = observed_package_cafs(&mut resident, public_b);
    assert!(assert_unchanged_public_caf(&public_after_cutoff, forced_key, first_caf).evaluated);
    eprintln!(
        "PACKAGE_CAF_HISTORY {:?} I_A={:?} I_B={:?} public_B={:?}",
        forced_key,
        forced_a[&forced_key].lease.instance(),
        forced_b[&forced_key].lease.instance(),
        public_after_cutoff[&forced_key].lease.instance()
    );
    drop(resident);
    drop(installation_b);
    drop(installation_a);
    drop(unpublished_b);
    drop(source_a);
}

fn public_package_cell(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    name: &str,
    expression: &str,
    expected: i64,
) {
    let public = resident.public_context();
    let execution = begin_fixture_execution(resident);
    let private = execution.private_scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: private,
            ..public
        })
        .unwrap();
    let source = format!("{name} <- pure (({expression}), T.pack (P.show ({expression})))");
    let (bound, compiled, reservation) =
        compile_checked_binding(resident, recipe, &source, execution.clone());
    let [binder] = bound.as_slice() else {
        panic!("one actual package-using public binder")
    };
    let completed = source_action(resident, |resident, settlement| {
        resident.run_bind_with_sites(
            name,
            compiled.code(),
            binder,
            reservation.generation(),
            settlement,
        )
    })
    .unwrap();
    let ResidentOutcome::Completed { result, .. } = completed else {
        panic!("the checked public-B package cell must execute")
    };
    assert_eq!(
        result.to_json(),
        serde_json::json!([expected, expected.to_string()])
    );
    publish_checked_fixture(resident, &execution, 1);
    resident.set_run_context(public).unwrap();
    drop(compiled);
    drop(reservation);
    drop(execution);
    resident.retire_scope(private);
}

fn prepare_package_source(
    resident: &mut TestSession,
    prepared: &crate::session::PreparedSourceEntry,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
) -> crate::session::PreparedSourceStartupEntry {
    let capsule = source_action_with_original(
        &mut resident.resident,
        source.original_source().clone(),
        |resident, _| resident.prepare_source_startup_entry(prepared),
    )
    .unwrap();
    assert_ne!(capsule.installation_scope().scope(), source.scope());
    capsule
}

fn run_package_source(
    resident: &mut TestSession,
    capsule: crate::session::PreparedSourceStartupEntry,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
) {
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: capsule.installation_scope().scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let completed = source_action_with_original(
        &mut resident.resident,
        source.original_source().clone(),
        |resident, settlement| resident.run_source_startup_entry(capsule, settlement),
    )
    .unwrap();
    let ResidentOutcome::Completed { result, .. } = completed else {
        panic!("the genuine source startup must complete its pure package demand");
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
}
