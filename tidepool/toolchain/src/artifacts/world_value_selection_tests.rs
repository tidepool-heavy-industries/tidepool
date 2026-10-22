use super::*;
use crate::cell_plan::WorldCellAdmissionSubject;
use crate::checked_cell::{CheckedPlannedCellSlot, ProducedValueTypeInterfaces};
use crate::compiler_world::{
    CapturedCompilerSourceRoot, CapturedCompilerSources, CheckedWorldCellProgram, CompilerWorld,
    CompilerWorldBindings, CompilerWorldCellOperand, CompilerWorldLexicalSurface,
    WorldCellOperandExposure, WorldSourceSelection,
};
use crate::linked_native::{AcquiredNativeImport, NativeCodeGrant};
use crate::recovery_artifacts::PackageInterfaceValidation;
use crate::toolchain::AdmittedCompilerEndpoint;
use tidepool_extract_cmd::{
    settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
    CompilerTransactionClose, CompilerTransactionOrigin, HostAdmissionCancellation,
};
use tidepool_repr::native_code::NativeCodeBody;

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
    let owner = crate::declaration_join::ExactModuleIdentity {
        unit: "main".into(),
        module: "CachedPackageWorldHome".into(),
    };
    let primary = Arc::new(
        crate::declaration_join::ExactDeclarationContext::from_issued_output(
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                &compiled_home
                    .producer_identity
                    .expect("the production compiler binds its producer identity"),
            )
            .sha256(),
            compiled_home
                .issued_output
                .as_ref()
                .expect("fresh compiler selection"),
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
        let (endpoint, world, source) = open_empty_world_with_primary(Some(Arc::clone(&primary)))?;
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
    let home_rows = selection
        .completion()
        .rows()
        .filter(|row| row.product().owner().module == "CachedPackageWorldHome")
        .collect::<Vec<_>>();
    assert!(
        !home_rows.is_empty(),
        "the World target selects Original HOME code"
    );
    for row in &home_rows {
        let offered = primary
            .compiler_source_support()
            .native_availability()
            .issued_row(row.binding().binding.artifact())
            .expect("the selected group belongs to the actual Original input");
        assert!(
            offered.exact_group_support(row.binding()).is_some(),
            "the World completion must retain the input's exact group issuer"
        );
        let issued = products
            .target_native_inputs()
            .certified_groups()
            .get(row.binding())
            .expect("the actual target certifier owns the selected group before completion");
        assert!(
            row.product().same_issuance(issued),
            "native completion preserves the target's selected issuing product"
        );
    }
    let packages = home_rows
        .iter()
        .flat_map(|row| row.imports())
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

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn prior_source_home_links_bind_first_packagefree_and_packageful_targets() {
    let mut ingress = Vec::new();
    let mut manifest_stages = BTreeMap::new();
    let ((prepared, checked), admission) = settle(|| {
        // Typed declaration stages import Display in their generated scaffold.
        let (endpoint, world, source) = open_world_with_primary_and_module(
            None,
            include_bytes!("../../tests/fixtures/world-native/PriorSourceModule.hs"),
            Some(capture_declared_scaffold_sources()?),
        )?;
        let template = include_str!("../../tests/fixtures/world-native/CellTemplate.hs").replace(
            "{{CELL_IMPORTS}}",
            "import qualified Module\n{{CELL_IMPORTS}}",
        );
        let plan = crate::compiler_world::plan_cell(
            &endpoint,
            &world,
            Arc::new(tidepool_extract_cmd::WorldCellRecipe {
                source: include_str!(
                    "../../tests/fixtures/world-native/PriorSourceFirstDemands.hs"
                )
                .into(),
                template,
                turn_templates: Vec::new(),
            }),
            None,
        )?;
        assert_eq!(plan.items().len(), 5);
        let origin = CompilerTransactionOrigin::current().unwrap();
        let subject = WorldCellAdmissionSubject::for_source_transaction(
            &origin,
            [19; 16],
            0,
            1,
            1,
            [b"prior Source first-demand control".to_vec()],
        )?;
        let slots = vec![
            CheckedPlannedCellSlot::Bind { value: 71 },
            CheckedPlannedCellSlot::Declaration { declaration: 72 },
            CheckedPlannedCellSlot::Bind { value: 73 },
            CheckedPlannedCellSlot::Declaration { declaration: 74 },
            CheckedPlannedCellSlot::Bind { value: 75 },
        ];
        let mut binders = vec![Vec::new(); 5];
        for (index, generation, name) in [
            (0, 71, "priorSeed"),
            (2, 73, "priorPlain"),
            (4, 75, "priorText"),
        ] {
            binders[index].push((
                name.to_owned(),
                tidepool_repr::SessionVarId::from_extract(
                    tidepool_codegen::prepared_program::session_var_id(
                        &tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation))
                            .module_name(),
                        name,
                    ),
                ),
            ));
        }
        let reservation = plan.reserve(plan.issue_admission_token(subject, slots, binders)?)?;
        let checked = crate::compiler_world::admit_world_cell_program(
            &endpoint,
            reservation,
            source.clone(),
            source,
            &origin,
        )?
        .map_err(|refusal| {
            CompileError::ExtractFailed(format!("prior Source World producer refused: {refusal:?}"))
        })?;
        assert_eq!(checked.stages().len(), 5);
        let prepared = prepare_world_cell_program_observing_home_contracts(
            Arc::clone(&checked),
            &mut |stage, originals| {
                let [ordinal] = checked.stages()[stage].ordinals() else {
                    panic!("one real checked item per first-demand fixture stage")
                };
                manifest_stages.insert(*ordinal, *originals.package_links().stage_id());
                if checked
                    .item(*ordinal)
                    .expect("actual checked stage item")
                    .executable()
                    .is_some()
                {
                    ingress.push((*ordinal, originals.assert_home_contract_ingress_controls()));
                }
            },
        )?;
        Ok((prepared, checked))
    });
    let program = prepared
        .seal(&admission)
        .expect("actual prior Source program END");
    assert_eq!(program.items().len(), 5);
    let mut unused_current = 0;
    let mut unused_prior = 0;
    let mut ordered_refusals = 0;
    let mut count_refusals = 0;
    for (item_ordinal, controls) in ingress {
        ordered_refusals += controls.ordered_refusals;
        count_refusals += controls.count_refusals;
        for (group, prior) in controls.unused {
            let target = program.items()[item_ordinal]
                .native_products()
                .expect("unused annotation control belongs to an executable stage")
                .linked_native_target()
                .unwrap();
            assert!(
                !target.acquired().any(|acquired| matches!(acquired.grant(), NativeCodeGrant::Home { binding, .. }
                    if binding.binding.artifact() == group.artifact && binding.original_ordinal == group.original_ordinal)),
                "the actual acquired closure contains no binding for the unused physical group"
            );
            if prior {
                unused_prior += 1;
            } else {
                unused_current += 1;
            }
        }
    }
    assert!(unused_current > 0, "actual unused CURRENT group premise");
    assert!(unused_prior > 0, "actual unused PRIOR group premise");
    assert!(
        count_refusals > 0,
        "actual package occurrence for empty-contract count refusal"
    );
    assert!(
        ordered_refusals > 0,
        "actual distinct ordered package-owner premise"
    );
    // Raw unused annotations above enter the actual issuer, while this complete
    // native history uses the producer's original transport. This is not a
    // transport mutation or an injected-prior end-to-end metamorphic control.
    for (index, packageful) in [(2, false), (4, true)] {
        let products = program.items()[index].native_products().unwrap();
        let target = products.linked_native_target().unwrap();
        let actual_binder = products
            .pending_imports()
            .iter()
            .find_map(|owner| match owner {
                certified_products::PendingImportOwner::Source { owner, binder, .. }
                    if owner.module == "Module" =>
                {
                    Some(binder)
                }
                _ => None,
            })
            .expect("target carries its actual prior Source binder");
        assert!(
            target
                .acquired()
                .any(|acquired| match acquired.code().body() {
                    NativeCodeBody::Home(body) =>
                        body.owner().module == "Module"
                            && body
                                .exported_values()
                                .any(|(binder, _)| binder == actual_binder),
                    _ => false,
                }),
            "actual non-inline prior Source export must be selected"
        );
        let mut packages = 0;
        for acquired in target.acquired() {
            let NativeCodeBody::Home(body) = acquired.code().body() else {
                continue;
            };
            if body.owner().module != "Module" {
                continue;
            }
            let NativeCodeGrant::Home {
                binding, product, ..
            } = acquired.grant()
            else {
                panic!("HOME body retains actual HOME grant")
            };
            let exact = products
                .target_native_inputs()
                .certified_groups()
                .get(binding)
                .expect("selected prior Source group has its actual physical binding");
            assert!(
                product.same_issuance(exact),
                "acquisition preserves exact prior physical product and issuer"
            );
            assert_eq!(body.owner(), exact.owner());
            assert_eq!(body.original_ordinal(), exact.group().original_ordinal());
            packages += acquired.imports().filter(|import| matches!(import, AcquiredNativeImport::Code { acquired, .. } if matches!(acquired.code().body(), NativeCodeBody::Package(_)))).count();
        }
        assert_eq!(packages > 0, packageful, "optimized package slot premise");
    }
    let first = program.items()[0]
        .native_products()
        .unwrap()
        .linked_native_target()
        .unwrap();
    assert!(!first.acquired().any(|acquired| matches!(acquired.code().body(), NativeCodeBody::Home(body) if body.owner().module == "Module")), "initial target does not demand later bodies");
    let mut previous = Vec::<Arc<crate::linked_native::LinkedNativeTarget>>::new();
    let mut borrowed_cohorts = 0;
    for (item_ordinal, manifest_stage) in manifest_stages {
        let Some(products) = program.items()[item_ordinal].native_products() else {
            continue;
        };
        let target = products
            .linked_native_target()
            .expect("actual executable stage target");
        for prior in &previous {
            borrowed_cohorts += crate::linked_native::tests::assert_borrowed_cohort_witnesses(
                prior,
                target,
                manifest_stage,
            );
        }
        previous.push(Arc::clone(target));
    }
    assert!(borrowed_cohorts > 0, "actual stages must share a package-bearing HOME head while the incoming package manifest changes");
    let request = checked.native_demand_completion();
    let stage = &checked.stages()[0];
    let unknown = Arc::new(stage.as_ref().clone());
    let validation =
        PackageInterfaceValidation::with_inventory(Arc::clone(&checked.validation().inventory));
    assert!(
        request
            .checked_stage(&checked, &unknown, &validation)
            .is_err(),
        "equal cloned stage observations do not issue a stage"
    );
    assert!(
        request
            .checked_stage(&checked, stage, &PackageInterfaceValidation::default())
            .is_err(),
        "another operation cannot supply the predecessor witness"
    );
}

fn open_empty_world() -> Result<
    (
        AdmittedCompilerEndpoint,
        Arc<CompilerWorld>,
        WorldSourceSelection,
    ),
    CompileError,
> {
    open_empty_world_with_primary(None)
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
    open_world_with_primary_and_module(primary, b"module Module where\n", None)
}

fn capture_declared_scaffold_sources() -> Result<CapturedCompilerSourceRoot, CompileError> {
    use std::io::Read;

    let root = PathBuf::from(
        std::env::var_os(crate::toolchain::ENV_PRELUDE_DIR).ok_or_else(|| {
            CompileError::ExtractFailed("missing declared World scaffold source resource".into())
        })?,
    );
    let files = crate::cache::source_root_manifest(&root)
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let mut remaining = CapturedCompilerSources::capture_byte_allowance()?;
    let mut bodies = BTreeMap::new();
    for (relative, digest) in &files {
        let path = root.join(relative);
        let length = std::fs::metadata(&path)?.len();
        let framed = 8u64
            .checked_add(relative.as_os_str().len() as u64)
            .and_then(|size| size.checked_add(length))
            .ok_or_else(|| {
                CompileError::ExtractFailed("World scaffold source size overflow".into())
            })?;
        remaining = remaining.checked_sub(framed).ok_or_else(|| {
            CompileError::ExtractFailed(
                "declared World scaffold sources exceed capture allowance".into(),
            )
        })?;
        let mut body = Vec::new();
        std::fs::File::open(&path)?
            .take(length.saturating_add(1))
            .read_to_end(&mut body)?;
        if body.len() as u64 != length || blake3::hash(&body).to_hex().as_str() != digest {
            return Err(CompileError::ExtractFailed(
                "declared World scaffold source changed during capture".into(),
            ));
        }
        bodies.insert(relative.clone(), Arc::<[u8]>::from(body));
    }
    if !bodies.contains_key(Path::new("Tidepool/Inspection/Display.hs")) {
        return Err(CompileError::ExtractFailed(
            "declared World scaffold sources lack Tidepool.Inspection.Display".into(),
        ));
    }
    let manifest = crate::cache::SourceRootManifest::from_file_digests(files)
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    Ok(CapturedCompilerSourceRoot { manifest, bodies })
}

fn open_world_with_primary_and_module(
    primary: Option<Arc<crate::declaration_join::ExactDeclarationContext>>,
    body: &[u8],
    scaffold: Option<CapturedCompilerSourceRoot>,
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
    let root = Arc::new(test_support::compiler_input_directory()?);
    std::fs::write(root.path().join("Module.hs"), body)?;
    let mut roots = vec![CapturedCompilerSourceRoot {
        manifest: crate::cache::SourceRootManifest::from_file_digests([(
            PathBuf::from("Module.hs"),
            blake3::hash(body).to_hex().to_string(),
        )])
        .expect("actual captured source manifest"),
        bodies: BTreeMap::from([(PathBuf::from("Module.hs"), Arc::<[u8]>::from(body))]),
    }];
    roots.extend(scaffold);
    let sources = Arc::new(CapturedCompilerSources::from_captured_roots(
        roots,
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
            certified.source_selection().interface_only_artifact(&owner),
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
        .compiler_inputs()
        .expect("first compiler owner")
        .role_projection()
        .snapshot_roles()
        .contains_key(&owner));
    assert_eq!(
        second
            .compiler_inputs()
            .expect("second compiler owner")
            .role_projection()
            .snapshot_roles()
            .get(&owner)
            .map(|role| role.interface()),
        Some(later)
    );
    Ok(())
}
