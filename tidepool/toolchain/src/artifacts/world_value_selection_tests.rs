use super::*;
use crate::cell_plan::WorldCellAdmissionSubject;
use crate::checked_cell::{CheckedPlannedCellSlot, ProducedValueTypeInterfaces};
use crate::compiler_world::{
    CapturedCompilerSourceRoot, CapturedCompilerSources, CheckedWorldCellProgram, CompilerWorld,
    CompilerWorldBindings, CompilerWorldCellOperand, CompilerWorldLexicalSurface,
    WorldCellOperandExposure, WorldSourceSelection,
};
use crate::recovery_artifacts::PackageInterfaceValidation;
use crate::toolchain::AdmittedCompilerEndpoint;
use tidepool_extract_cmd::{
    settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
    CompilerTransactionClose, CompilerTransactionOrigin, HostAdmissionCancellation,
};

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_world_value_selection_test owns it"]
fn real_world_value_selection_preserves_physical_owner_and_item_prefix() {
    let ((endpoint, world, source, prepared), admission) = settle(|| {
        let (endpoint, world, source) = open_empty_world()?;
        let checked = prepare_checked(
            &endpoint,
            &world,
            &source,
            "let supplied = (7 :: Int)",
            None,
            &[(41, "supplied")],
        )?;
        let prepared = prepare_world_cell_program(checked)?;
        Ok((endpoint, world, source, prepared))
    });
    let supplied = prepared
        .seal(&admission)
        .expect("actual input compiler END");
    let execution = supplied.items()[0]
        .native()
        .expect("actual input native evidence")
        .clone();
    let operand = CompilerWorldCellOperand::from_checked_host_execution(
        &world,
        WorldCellOperandExposure::Input,
        1,
        &execution,
    )
    .expect("operand from the actual checked execution owner");

    for used in [false, true] {
        let (prepared, admission) = settle(|| {
            let checked = prepare_checked(
                &endpoint,
                &world,
                &source,
                if used {
                    include_str!("../../tests/fixtures/world-native/SelectionUsed.hs")
                } else {
                    include_str!("../../tests/fixtures/world-native/SelectionUnused.hs")
                },
                Some(operand.clone()),
                &[(51, "currentSelection"), (52, "laterSelection")],
            )?;
            assert_eq!(checked.stages().len(), 1);
            assert_eq!(checked.stages()[0].ordinals(), &[0, 1]);
            let demand = checked.runtime_binding_demand();
            assert!(demand.prior_stage().is_empty());
            if used {
                assert_eq!(demand.existing(), &[operand.session_var()]);
            } else {
                assert!(demand.existing().is_empty());
                assert!(checked.stages()[0].demanded_rows().is_empty());
            }
            assert_physical_selection(&checked, &operand)?;
            prepare_world_cell_program(checked)
        });
        let program = prepared
            .seal(&admission)
            .expect("actual selected native END");
        let first = program.items()[0]
            .native_products()
            .expect("first target owner");
        let second = program.items()[1]
            .native_products()
            .expect("second target owner");
        let first_bindings = first.target.artifacts().selected_native_binding_rows().1;
        let second_bindings = second.target.artifacts().selected_native_binding_rows().1;
        let first_packages = first
            .target
            .completed_selection()
            .expect("first item has package completion")
            .target_imports()
            .iter()
            .filter_map(|import| match import {
                crate::artifact_inventory::CompletedNativeTargetImport::Package {
                    binding, ..
                } => Some((
                    binding.product().code().owner().unit.clone(),
                    binding.product().code().owner().module.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            !first_packages.is_empty(),
            "the World declaration item must carry its actual Data.Text package roots"
        );
        assert!(
            !first_bindings.is_empty(),
            "first item selects executable code"
        );
        for binding in first_bindings {
            assert!(
                second_bindings.contains(&binding),
                "ordered native progress must retain the prior target's exact binding"
            );
            assert!(second.target.certified_groups().get(binding).is_some());
        }
        let requires_operand = program.items().iter().any(|item| {
            item.native()
                .expect("executable item")
                .required_native_imports()
                .any(|(symbol, generation)| {
                    symbol == operand.symbol() && generation == operand.generation()
                })
        });
        assert_eq!(requires_operand, used);
    }
}

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_package_occurrence_linkage_tests owns it"]
fn real_world_package_item_keeps_exact_roots_for_a_later_cell() {
    let ((endpoint, world, source, prepared), admission) = settle(|| {
        let (endpoint, world, source) = open_empty_world()?;
        let checked = prepare_package_checked(
            &endpoint,
            &world,
            &source,
            include_str!("../../tests/fixtures/world-native/PackageWorldItem.hs"),
            &[(41, "supplied")],
        )?;
        assert_eq!(checked.stages().len(), 1);
        assert_eq!(checked.stages()[0].ordinals(), &[0]);
        let prepared = prepare_world_cell_program(checked)?;
        Ok((endpoint, world, source, prepared))
    });
    let first = prepared
        .seal(&admission)
        .expect("package-using World item reaches native END");
    assert_eq!(first.items().len(), 1);
    let products = first.items()[0]
        .native_products()
        .expect("package item owns native products");
    let selection = products
        .completed_native_selection()
        .expect("package item owns its exact completed selection");
    assert_eq!(
        products
            .native_source_products()
            .expect("item source products resolve through the item issuer")
            .target()
            .definitions()
            .globals()
            .len(),
        selection.target_imports().len(),
        "the item target has exactly one selected link per physical global"
    );
    let package_roots = selection
        .target_imports()
        .iter()
        .filter_map(|import| match import {
            crate::artifact_inventory::CompletedNativeTargetImport::Package { binding, .. } => {
                Some(format!(
                    "{}:{}",
                    binding.product().code().owner().unit,
                    binding.product().code().owner().module
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        package_roots.len(),
        3,
        "Data.List.sort retains its three ordered package roots: {package_roots:?}"
    );
    assert!(
        package_roots.iter().all(|owner| owner.ends_with("Resume")),
        "the ordered roots retain their issued package owner: {package_roots:?}"
    );
    let home_rows = selection
        .completion()
        .rows()
        .filter(|row| {
            row.imports()
                .iter()
                .filter_map(|import| match import {
                    crate::artifact_inventory::CompletedNativeImport::Package {
                        binding, ..
                    } => Some(binding.product().code().owner().module.as_str()),
                    _ => None,
                })
                .eq(["GHC.Classes", "GHC.Internal.Data.OldList"])
        })
        .collect::<Vec<_>>();
    assert_eq!(home_rows.len(), 1, "one package-using HOME declaration");
    let home = home_rows[0];
    assert_eq!(home.binding().original_ordinal, 0);
    let home_packages = home
        .imports()
        .iter()
        .filter_map(|import| match import {
            crate::artifact_inventory::CompletedNativeImport::Package { binding, .. } => {
                Some(binding)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(home_packages.len(), 2);
    assert_eq!(
        home_packages[0].product().code().owner().module,
        "GHC.Classes"
    );
    assert_eq!(
        home_packages[1].product().code().owner().module,
        "GHC.Internal.Data.OldList"
    );
    assert_eq!(home_packages[1].imports().len(), 2);
    let transitive_home_rows = selection
        .completion()
        .rows()
        .filter(|row| {
            row.imports()
                .iter()
                .filter_map(|import| match import {
                    crate::artifact_inventory::CompletedNativeImport::Package {
                        binding, ..
                    } => Some(binding.product().code().owner().module.as_str()),
                    _ => None,
                })
                .eq(["Tidepool.Internal.Resume", "Tidepool.Internal.Resume"])
        })
        .collect::<Vec<_>>();
    assert_eq!(
        transitive_home_rows.len(),
        1,
        "the transitively reached HOME row retains its package obligations"
    );
    let transitive_packages = transitive_home_rows[0]
        .imports()
        .iter()
        .filter_map(|import| match import {
            crate::artifact_inventory::CompletedNativeImport::Package { binding, .. } => {
                Some(binding.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transitive_packages.len(), 2);
    assert_ne!(
        transitive_packages[0].binding(),
        transitive_packages[1].binding(),
        "the transitive HOME row keeps both exact package bindings"
    );
    let execution = first.items()[0]
        .native()
        .expect("package input item is executable")
        .clone();
    let operand = CompilerWorldCellOperand::from_checked_host_execution(
        &world,
        WorldCellOperandExposure::Input,
        1,
        &execution,
    )
    .expect("the package result is captured by the later-cell operand owner");
    let (prepared, admission) = settle(|| {
        let body = include_str!("../../tests/fixtures/world-native/PackageWorldConsumer.hs")
            .replace("{{OPERAND_BINDER}}", &operand.symbol().occurrence);
        let checked = prepare_checked(
            &endpoint,
            &world,
            &source,
            &body,
            Some(operand),
            &[(51, "laterValue")],
        )?;
        assert_eq!(checked.stages().len(), 1);
        assert_eq!(checked.stages()[0].ordinals(), &[0]);
        prepare_world_cell_program(checked)
    });
    let later = prepared
        .seal(&admission)
        .expect("later World cell consumes the package-derived value");
    assert_eq!(later.items().len(), 1);
    let later_products = later.items()[0]
        .native_products()
        .expect("later item owns native products");
    assert!(
        later_products
            .completed_native_selection()
            .expect("later item owns its exact completion")
            .target_imports()
            .iter()
            .all(|import| match import {
                crate::artifact_inventory::CompletedNativeTargetImport::Package {
                    binding, ..
                } => {
                    let module = &binding.product().code().owner().module;
                    module != "GHC.Classes" && module != "GHC.Internal.Data.OldList"
                }
                _ => true,
            }),
        "the later cell consumes the issued value, not a package import chosen by name"
    );
}

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_package_occurrence_linkage_tests owns it"]
fn real_cached_home_world_item_admits_its_package_roots() {
    let home_source = include_str!("../../tests/fixtures/world-native/CachedPackageWorldHome.hs");
    let compiled_home = crate::artifacts::test_support::compile_invocation(
        &crate::artifacts::CompileInvocation {
            source: home_source,
            targets: &["cachedTextLength"],
            include: &[],
            fallback_module_name: "CachedPackageWorldHome",
        },
        |_, _, _| {},
    )
    .expect("the cached HOME owner comes from the production compiler");
    let issued_rows = compiled_home.targets["cachedTextLength"]
        .completed_native_selection()
        .expect("the initial compiler target owns a native completion")
        .completion()
        .rows()
        .filter(|row| {
            row.product()
                .group()
                .binders()
                .iter()
                .any(|binder| binder.occurrence == "cachedTextLength")
        })
        .collect::<Vec<_>>();
    let [issued] = issued_rows.as_slice() else {
        panic!("the initial target must issue one cachedTextLength HOME group");
    };
    let owner = crate::declaration_join::ExactModuleIdentity {
        unit: "main".into(),
        module: "CachedPackageWorldHome".into(),
    };
    let primary = Arc::new(
        crate::declaration_join::ExactDeclarationContext::from_source_selection(
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                &compiled_home
                    .producer_identity
                    .expect("the production compiler binds its producer identity"),
            )
            .sha256(),
            compiled_home
                .source_selection
                .as_ref()
                .expect("fresh compiler selection"),
            &compiled_home.artifact_view,
            vec![crate::declaration_join::ExactLexicalNode {
                owner: owner.clone(),
                imports: Vec::new(),
            }],
            owner.clone(),
            &[owner],
        )
        .unwrap(),
    );
    let ((_endpoint, _world, _source, prepared), admission) = settle(|| {
        let (endpoint, world, source) = open_empty_world_with_primary(Some(primary))?;
        let template = include_str!("../../tests/fixtures/world-native/PackageCellTemplate.hs")
            .replace(
                "{{CELL_IMPORTS}}",
                "import qualified CachedPackageWorldHome as Cached\n{{CELL_IMPORTS}}",
            );
        let checked = prepare_checked_with_template(
            &endpoint,
            &world,
            &source,
            include_str!("../../tests/fixtures/world-native/CachedPackageWorldItem.hs").into(),
            None,
            &[(41, "cachedWorldValue")],
            template,
        )?;
        assert_eq!(checked.stages().len(), 1);
        let prepared = prepare_world_cell_program(checked)?;
        Ok((endpoint, world, source, prepared))
    });
    let output = prepared
        .seal(&admission)
        .expect("the cached HOME World item reaches native END");
    crate::certified_products::assert_actual_world_original_package_refusals(&output);
    let products = output.items()[0]
        .native_products()
        .expect("cached HOME item owns native products");
    let selection = products
        .completed_native_selection()
        .expect("cached HOME item owns its completed selection");
    let row = selection
        .completion()
        .rows()
        .find(|row| row.binding() == issued.binding())
        .expect("the World target preserves the initial HOME group's exact binding");
    assert!(
        row.product().same_issuance(issued.product()),
        "the World target preserves the original group's issuing product"
    );
    let packages = row
        .imports()
        .iter()
        .filter_map(|import| match import {
            crate::artifact_inventory::CompletedNativeImport::Package { binding, .. } => {
                Some(binding.product().code().owner().module.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        packages
            .iter()
            .any(|module| *module == "Data.Text.Internal"),
        "the cached HOME row retains its Data.Text package roots: {packages:?}"
    );
    let source_products = products
        .native_source_products()
        .expect("cached item has current source products");
    assert!(
        !source_products.target_requirements().is_empty(),
        "the final source target retains its ordered cached HOME dependency slots"
    );
}

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_world_reference_history_test owns it"]
fn real_world_reference_history_restores_same_owner_and_rejects_crossed_ready() {
    use crate::host_work::HostWorkCounter as Counter;
    assert_eq!(
        std::env::var("TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES").as_deref(),
        Ok("0")
    );
    let observation = crate::host_work::HostWorkObservation::new();
    observation.scope(|| {
        let ((endpoint, world, _), _) = settle(open_empty_world);
        let initial = observation.snapshot();
        assert_eq!(initial.get(Counter::WorldEnsureResidentProbe), 0);
        assert_eq!(initial.get(Counter::WorldEnsureComplete), 1);
        assert!(matches!(world.last_admission_residency().unwrap(), Some(tidepool_extract_cmd::WorldResidency::ActiveOnly(_))));
        let logical_world = *world.logical_id();
        for _ in 0..2 {
            let before = observation.snapshot();
            settle(|| {
                let answer = crate::compiler_world::query_public_module(&endpoint, &world,
                    crate::compiler_world::PublicModuleQuestion::structured_type(
                        "Data.List".into(), tidepool_extract_cmd::InspectionNamespace::Value, "map".into()))?;
                assert!(Arc::ptr_eq(answer.world(), &world));
                assert!(matches!(answer.answer(), crate::compiler_world::WorldAnswer::Type { .. }), "actual query answer: {:?}", answer.answer());
                Ok(())
            });
            let after = observation.snapshot();
            assert_eq!(after.get(Counter::WorldEnsureResidentProbe), before.get(Counter::WorldEnsureResidentProbe) + 1);
            assert_eq!(after.get(Counter::WorldEnsureNeedsAcquisition), before.get(Counter::WorldEnsureNeedsAcquisition) + 1);
            assert_eq!(after.get(Counter::WorldEnsureComplete), before.get(Counter::WorldEnsureComplete) + 1);
            assert_eq!(world.logical_id(), &logical_world);
            assert!(matches!(world.last_admission_residency().unwrap(), Some(tidepool_extract_cmd::WorldResidency::ActiveOnly(_))));
        }
        let crossed = settle_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(), |_| {},
            || crate::compiler_world::assert_actual_world_reference_pairing(&world, endpoint.identity()));
        assert!(crossed.action().is_ok(), "actual response mutation control: {:?}", crossed.action());
        assert!(matches!(crossed.close(), CompilerTransactionClose::Retired(evidence)
            if evidence.reason == tidepool_extract_cmd::CompilerTransactionCloseReason::FailedRequest),
            "crossed response must poison its actual transaction: {:?}", crossed.close());
    });
}

fn open_empty_world() -> Result<
    (
        AdmittedCompilerEndpoint,
        Arc<CompilerWorld>,
        WorldSourceSelection,
    ),
    CompileError,
> {
    let configured = crate::toolchain::CompilerDeploymentConfiguration::from_env()
        .expect("declared compiler deployment");
    let (endpoint, _, _) = crate::toolchain::bind_admitted_extract_endpoint(&configured)
        .expect("configured compiler endpoint");
    let endpoint =
        AdmittedCompilerEndpoint::from_bound(endpoint).expect("admitted compiler endpoint");
    let root = Arc::new(tempfile::tempdir()?);
    let body = b"module Module where\n";
    std::fs::write(root.path().join("Module.hs"), body)?;
    let sources = Arc::new(CapturedCompilerSources::from_captured_roots(
        vec![CapturedCompilerSourceRoot {
            manifest: crate::cache::SourceRootManifest::from_file_digests([(
                PathBuf::from("Module.hs"),
                blake3::hash(body).to_hex().to_string(),
            )])
            .expect("actual captured source manifest"),
            bodies: BTreeMap::from([(
                PathBuf::from("Module.hs"),
                Arc::<[u8]>::from(body.as_slice()),
            )]),
        }],
        Arc::from(&b"world-selection-source"[..]),
        root,
    )?);
    let source = WorldSourceSelection::new(*sources.seal(), "world-selection-source")?;
    let inputs = ModuleCandidateOffer::compiler_world_inputs(
        None,
        CompilerWorldBindings::from_published_bindings(0, std::iter::empty())?,
        sources,
        CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None)?,
    )?;
    let world = crate::compiler_world::open_world(inputs, &endpoint)?;
    Ok((endpoint, world, source))
}

fn open_empty_world_with_primary(
    primary: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
) -> Result<
    (
        AdmittedCompilerEndpoint,
        Arc<CompilerWorld>,
        WorldSourceSelection,
    ),
    CompileError,
> {
    let configured = crate::toolchain::CompilerDeploymentConfiguration::from_env()
        .expect("declared compiler deployment");
    let (endpoint, _, _) = crate::toolchain::bind_admitted_extract_endpoint(&configured)
        .expect("configured compiler endpoint");
    let endpoint =
        AdmittedCompilerEndpoint::from_bound(endpoint).expect("admitted compiler endpoint");
    let root = Arc::new(tempfile::tempdir()?);
    let body = b"module Module where\n";
    std::fs::write(root.path().join("Module.hs"), body)?;
    let sources = Arc::new(CapturedCompilerSources::from_captured_roots(
        vec![CapturedCompilerSourceRoot {
            manifest: crate::cache::SourceRootManifest::from_file_digests([(
                PathBuf::from("Module.hs"),
                blake3::hash(body).to_hex().to_string(),
            )])
            .expect("actual captured source manifest"),
            bodies: BTreeMap::from([(
                PathBuf::from("Module.hs"),
                Arc::<[u8]>::from(body.as_slice()),
            )]),
        }],
        Arc::from(&b"world-selection-source"[..]),
        root,
    )?);
    let source = WorldSourceSelection::new(*sources.seal(), "world-selection-source")?;
    let inputs = ModuleCandidateOffer::compiler_world_inputs(
        primary,
        CompilerWorldBindings::from_published_bindings(0, std::iter::empty())?,
        sources,
        CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None)?,
    )?;
    let world = crate::compiler_world::open_world(inputs, &endpoint)?;
    Ok((endpoint, world, source))
}

fn settle<T>(action: impl FnOnce() -> Result<T, CompileError>) -> (T, HostAdmissionCancellation) {
    let settled = settle_compiler_transaction_cancellable(
        CompilerTransactionCancellation::new(),
        |_| {},
        action,
    );
    if let Err(error) = settled.action() {
        panic!(
            "actual compiler selection control failed: {error:?}; END: {:?}",
            settled.close()
        );
    }
    assert!(matches!(settled.close(), CompilerTransactionClose::Clean));
    let admission = settled
        .host_admission_cancellation()
        .expect("issued END")
        .clone();
    (
        settled
            .into_outcome()
            .action
            .expect("actual compiler output"),
        admission,
    )
}

fn prepare_checked(
    endpoint: &AdmittedCompilerEndpoint,
    world: &Arc<CompilerWorld>,
    source: &WorldSourceSelection,
    body: &str,
    operand: Option<Arc<CompilerWorldCellOperand>>,
    values: &[(u64, &str)],
) -> Result<Arc<CheckedWorldCellProgram>, CompileError> {
    let (body, template) = match operand.as_ref() {
        Some(operand) => {
            let symbol = operand.symbol();
            (
                body.replace("{{OPERAND_BINDER}}", &symbol.occurrence),
                include_str!("../../tests/fixtures/world-native/SelectionTemplate.hs")
                    .replace("{{OPERAND_MODULE}}", &symbol.module)
                    .replace("{{OPERAND_BINDER}}", &symbol.occurrence),
            )
        }
        None => (
            body.into(),
            include_str!("../../tests/fixtures/world-native/CellTemplate.hs").into(),
        ),
    };
    prepare_checked_with_template(endpoint, world, source, body, operand, values, template)
}

fn prepare_package_checked(
    endpoint: &AdmittedCompilerEndpoint,
    world: &Arc<CompilerWorld>,
    source: &WorldSourceSelection,
    body: &str,
    values: &[(u64, &str)],
) -> Result<Arc<CheckedWorldCellProgram>, CompileError> {
    prepare_checked_with_template(
        endpoint,
        world,
        source,
        body.into(),
        None,
        values,
        include_str!("../../tests/fixtures/world-native/PackageCellTemplate.hs").into(),
    )
}

fn prepare_checked_with_template(
    endpoint: &AdmittedCompilerEndpoint,
    world: &Arc<CompilerWorld>,
    source: &WorldSourceSelection,
    body: String,
    operand: Option<Arc<CompilerWorldCellOperand>>,
    values: &[(u64, &str)],
    template: String,
) -> Result<Arc<CheckedWorldCellProgram>, CompileError> {
    let plan = crate::compiler_world::plan_cell(
        endpoint,
        world,
        Arc::new(tidepool_extract_cmd::WorldCellRecipe {
            source: body,
            template,
            turn_templates: Vec::new(),
        }),
        operand,
    )?;
    assert_eq!(plan.items().len(), values.len());
    let origin = CompilerTransactionOrigin::current().expect("actual compiler transaction");
    let subject = WorldCellAdmissionSubject::for_source_transaction(
        &origin,
        [17; 16],
        0,
        1,
        1,
        [b"compiler-only physical selection control".to_vec()],
    )?;
    let slots = values
        .iter()
        .map(|(value, _)| CheckedPlannedCellSlot::Bind { value: *value })
        .collect();
    let binders = values
        .iter()
        .map(|(generation, name)| {
            vec![(
                (*name).into(),
                tidepool_repr::SessionVarId::from_extract(
                    tidepool_codegen::prepared_program::session_var_id(
                        &tidepool_repr::SessionModule::val(tidepool_repr::Generation(*generation))
                            .module_name(),
                        name,
                    ),
                ),
            )]
        })
        .collect();
    let reservation = plan.reserve(plan.issue_admission_token(subject, slots, binders)?)?;
    crate::compiler_world::admit_world_cell_program(
        endpoint,
        reservation,
        source.clone(),
        source.clone(),
        &origin,
    )?
    .map_err(|refusal| {
        CompileError::ExtractFailed(format!("actual World producer refused: {refusal:?}"))
    })
}

fn assert_physical_selection(
    checked: &Arc<CheckedWorldCellProgram>,
    operand: &CompilerWorldCellOperand,
) -> Result<(), CompileError> {
    let mut validation =
        PackageInterfaceValidation::with_inventory(checked.validation().inventory.clone());
    let produced = crate::checked_cell::CheckedValueInputs::capture_world_produced_types(
        checked,
        &mut validation,
    )?;
    let physical = produced.for_segment(0..2)?;
    let recaptured = crate::checked_cell::CheckedValueInputs::capture_world_produced_types(
        checked,
        &mut validation,
    )?;
    for generation in [51, 52] {
        assert_eq!(
            physical.current_interface(generation)?.artifact_id(),
            recaptured.current_interface(generation)?.artifact_id()
        );
    }
    let decoded = crate::declaration_context::decode_world_source_receipt(checked, 0)?;
    let segment = decoded.issue_segment_originals(
        None,
        &physical,
        &mut validation,
        crate::declaration_context::WorldProgramContinuation::begin(checked),
    )?;
    let input = operand.checked_row().certified_interface().artifact_id();
    assert!(
        segment
            .originals
            .value_entries()
            .iter()
            .any(|entry| entry.descriptor.id == input),
        "the actual helper RHS must select its operand interface before item projection"
    );
    let relative = PathBuf::from("item-0");
    let path = checked
        .item(0)
        .expect("first checked item")
        .executable()
        .expect("executable")
        .manifest()
        .keys()
        .find(|path| {
            path.to_str()
                .is_some_and(|path| path.ends_with(".prepared.cbor"))
        })
        .expect("actual prepared bytes");
    let prepared_bytes = crate::checked_cell::world_output_bytes(checked, &relative.join(path))?;
    let program_artifact = crate::prepared_artifact::PreparedArtifact::parse_shared(
        Arc::new(prepared_bytes.as_ref().to_vec()),
        &crate::prepared_artifact::production_requirements()?,
        DecodeLimits::default(),
    )?;
    let program = program_artifact.bind_package_program();
    let mut cursor = segment
        .originals
        .bound_artifacts()
        .map_err(compiler_evidence_failure)?;
    let foreign = recaptured.for_item(0)?;
    let empty = physical.for_segment(0..0)?;
    for selected in [None, Some(&foreign), Some(&physical), Some(&empty)] {
        let receipt = certified_products::decode_segment_item_with_operation(
            crate::checked_cell::world_output_bytes(
                checked,
                &relative.join("certified-products.cbor"),
            )?,
            &validation.inventory,
        )
        .map_err(compiler_evidence_failure)?;
        let result = segment.originals.select_item(
            &mut cursor,
            receipt,
            &program,
            selected,
            &mut validation,
        );
        assert!(matches!(
            result,
            Err(certified_products::CertificationError::Mismatch(
                "item produced type selection differs from its physical segment"
            ))
        ));
    }
    // A valid but later selector cannot skip the current checked item.
    let later_selector = physical.for_item(1)?;
    let receipt = certified_products::decode_segment_item_with_operation(
        crate::checked_cell::world_output_bytes(
            checked,
            &relative.join("certified-products.cbor"),
        )?,
        &validation.inventory,
    )
    .map_err(compiler_evidence_failure)?;
    assert!(matches!(
        segment.originals.select_item(
            &mut cursor,
            receipt,
            &program,
            Some(&later_selector),
            &mut validation,
        ),
        Err(certified_products::CertificationError::OriginalClosure(_))
    ));
    let first: ProducedValueTypeInterfaces = physical.for_item(0)?;
    let receipt = certified_products::decode_segment_item_with_operation(
        crate::checked_cell::world_output_bytes(
            checked,
            &relative.join("certified-products.cbor"),
        )?,
        &validation.inventory,
    )
    .map_err(compiler_evidence_failure)?;
    let (certified, _, _, _) = segment
        .originals
        .select_item(
            &mut cursor,
            receipt,
            &program,
            Some(&first),
            &mut validation,
        )
        .map_err(compiler_evidence_failure)?;
    let receipt = certified_products::decode_segment_item_with_operation(
        crate::checked_cell::world_output_bytes(
            checked,
            &relative.join("certified-products.cbor"),
        )?,
        &validation.inventory,
    )
    .map_err(compiler_evidence_failure)?;
    assert!(matches!(
        segment.originals.select_item(
            &mut cursor,
            receipt,
            &program,
            Some(&first),
            &mut validation,
        ),
        Err(certified_products::CertificationError::OriginalClosure(_))
    ));
    let current = physical.current_interface(51)?.artifact_id();
    let later = physical.current_interface(52)?.artifact_id();
    assert!(certified
        .value_interfaces
        .iter()
        .any(|interface| interface.artifact_id() == input));
    assert!(certified
        .value_interfaces
        .iter()
        .any(|interface| interface.artifact_id() == current));
    assert!(!certified
        .value_interfaces
        .iter()
        .any(|interface| interface.artifact_id() == later));
    for (generation, expected) in [(41, Some(input)), (51, Some(current)), (52, None)] {
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation))
                .module_name(),
        };
        assert_eq!(
            certified.source_selection.interface_only_artifact(&owner),
            expected
        );
    }
    let relative = PathBuf::from("item-1");
    let path = checked
        .item(1)
        .expect("second checked item")
        .executable()
        .expect("executable")
        .manifest()
        .keys()
        .find(|path| {
            path.to_str()
                .is_some_and(|path| path.ends_with(".prepared.cbor"))
        })
        .expect("actual second prepared bytes");
    let prepared_bytes = crate::checked_cell::world_output_bytes(checked, &relative.join(path))?;
    let program_artifact = crate::prepared_artifact::PreparedArtifact::parse_shared(
        Arc::new(prepared_bytes.as_ref().to_vec()),
        &crate::prepared_artifact::production_requirements()?,
        DecodeLimits::default(),
    )?;
    let program = program_artifact.bind_package_program();
    let receipt = certified_products::decode_segment_item_with_operation(
        crate::checked_cell::world_output_bytes(
            checked,
            &relative.join("certified-products.cbor"),
        )?,
        &validation.inventory,
    )
    .map_err(compiler_evidence_failure)?;
    let (second, _, _, _) = segment
        .originals
        .select_item(
            &mut cursor,
            receipt,
            &program,
            Some(&later_selector),
            &mut validation,
        )
        .map_err(compiler_evidence_failure)?;
    let owner = crate::declaration_join::ExactModuleIdentity {
        unit: "main".into(),
        module: tidepool_repr::SessionModule::val(tidepool_repr::Generation(52)).module_name(),
    };
    assert!(!certified
        .original_inputs
        .as_ref()
        .expect("first compiler owner")
        .role_projection()
        .snapshot_roles()
        .contains_key(&owner));
    assert_eq!(
        second
            .original_inputs
            .as_ref()
            .expect("second compiler owner")
            .role_projection()
            .snapshot_roles()
            .get(&owner)
            .map(|role| role.interface()),
        Some(later)
    );
    Ok(())
}
