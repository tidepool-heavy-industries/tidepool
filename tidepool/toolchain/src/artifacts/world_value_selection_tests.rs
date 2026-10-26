use super::*;
use crate::cell_plan::WorldCellAdmissionSubject;
use crate::checked_cell::{CheckedPlannedCellSlot, ProducedValueTypeInterfaces};
use crate::compiler_world::{
    CapturedCompilerSourceRoot, CapturedCompilerSources, CheckedWorldCellProgram, CompilerWorld,
    CompilerWorldCellOperand, CompilerWorldLexicalSurface, VisibleWorldBindings,
    WorldCellOperandExposure, WorldSourceSelection,
};
use crate::linked_native::{
    AcquiredNativeCode, AcquiredNativeImport, LinkedNativeTargetImport, NativeCodeGrant,
};
use crate::recovery_artifacts::PackageInterfaceValidation;
use crate::toolchain::AdmittedCompilerEndpoint;
use tidepool_extract_cmd::{
    settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
    CompilerTransactionClose, CompilerTransactionOrigin, HostAdmissionCancellation,
};
use tidepool_repr::native_code::NativeCodeBody;

fn package_children(acquired: &AcquiredNativeCode) -> Vec<AcquiredNativeCode> {
    acquired
        .imports()
        .filter_map(|import| match import {
            AcquiredNativeImport::Code { acquired, .. }
                if matches!(acquired.code().body(), NativeCodeBody::Package(_)) =>
            {
                Some(acquired)
            }
            _ => None,
        })
        .collect()
}

fn package_module(acquired: &AcquiredNativeCode) -> &str {
    let NativeCodeBody::Package(body) = acquired.code().body() else {
        panic!("actual package occurrence")
    };
    &body.owner().module
}

/// Compile and seal one real World value, retaining only its issued interface.
/// Call once outside generated histories; independent issuers require separate
/// calls. No cached program owner extends the returned artifact's lifetime.
pub(crate) fn prepare_checked_value_fixture(
    generation: u64,
) -> (Arc<crate::checked_cell::CheckedValueArtifact>, Vec<u8>) {
    let ((prepared, producer_bytes), admission) = settle(|| {
        let (endpoint, world, source) = open_empty_world()?;
        let producer_bytes = endpoint.identity().producer_bytes().to_vec();
        let checked = prepare_checked(
            &endpoint,
            &world,
            &source,
            "let supplied = (7 :: Int)",
            None,
            &[(generation, "supplied")],
        )?;
        Ok((prepare_world_cell_program(checked)?, producer_bytes))
    });
    let program = prepared
        .seal(&admission)
        .expect("actual value compiler END");
    assert_eq!(program.items().len(), 1);
    let value = program.items()[0]
        .native()
        .expect("actual value native evidence")
        .value_interface_certificate()
        .expect("actual World-issued value interface");
    assert_eq!(
        value.owner(),
        tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation))
    );
    (value, producer_bytes)
}

/// Capture nominal HOME TYPE custody and then a second checked value through
/// the first value's actual operand. Both products retain the same issued child
/// nominal bindings, without assuming that term demand selects a TYPE edge.
pub(crate) fn prepare_checked_value_dependency_pair_fixture(
    first_generation: u64,
    second_generation: u64,
) -> (
    Arc<crate::checked_cell::CheckedValueArtifact>,
    Arc<crate::checked_cell::CheckedValueArtifact>,
) {
    prepare_checked_value_type_pair(
        first_generation,
        second_generation,
        ValueTypeFixture::SharedNominal,
    )
}

pub(crate) fn prepare_checked_value_captured_type_pair_fixture(
    first_generation: u64,
    second_generation: u64,
) -> (
    Arc<crate::checked_cell::CheckedValueArtifact>,
    Arc<crate::checked_cell::CheckedValueArtifact>,
) {
    prepare_checked_value_type_pair(
        first_generation,
        second_generation,
        ValueTypeFixture::CapturedNominal,
    )
}

pub(crate) fn prepare_checked_value_historical_pair_fixture() -> (
    Arc<crate::checked_cell::CheckedValueArtifact>,
    Arc<crate::checked_cell::CheckedValueArtifact>,
) {
    let (first, _) = prepare_checked_value_type_pair(601, 602, ValueTypeFixture::SharedNominal);
    let (second, _) =
        prepare_checked_value_type_pair(603, 604, ValueTypeFixture::HistoricalNominal);
    (first, second)
}

enum ValueTypeFixture {
    SharedNominal,
    CapturedNominal,
    HistoricalNominal,
}

fn prepare_checked_value_type_pair(
    first_generation: u64,
    second_generation: u64,
    kind: ValueTypeFixture,
) -> (
    Arc<crate::checked_cell::CheckedValueArtifact>,
    Arc<crate::checked_cell::CheckedValueArtifact>,
) {
    let ((endpoint, world, source, prepared), admission) = settle(|| {
        let (endpoint, world, source) = open_world_with_primary_and_module(
            None,
            match kind {
                ValueTypeFixture::HistoricalNominal => {
                    include_str!("../../tests/fixtures/world-native/WorldTypeHistorical.hs")
                }
                _ => include_str!("../../tests/fixtures/world-native/WorldTypeNominal.hs"),
            }
            .as_bytes(),
            match kind {
                ValueTypeFixture::SharedNominal | ValueTypeFixture::HistoricalNominal => None,
                ValueTypeFixture::CapturedNominal => Some(capture_declared_scaffold_sources()?),
            },
        )?;
        let template = include_str!("../../tests/fixtures/world-native/CellTemplate.hs").replace(
            "{{CELL_IMPORTS}}",
            "import qualified Module as Nominal\n{{CELL_IMPORTS}}",
        );
        let checked = prepare_checked_with_template(
            &endpoint,
            &world,
            &source,
            "let supplied = Nominal.token".into(),
            None,
            &[(first_generation, "supplied")],
            template.into(),
        )?;
        Ok((
            endpoint,
            world,
            source,
            prepare_world_cell_program(checked)?,
        ))
    });
    let first = prepared
        .seal(&admission)
        .expect("actual nominal value compiler END");
    let execution = first.items()[0]
        .native()
        .expect("actual nominal value execution");
    let row = execution
        .value_interface_certificate()
        .expect("actual nominal checked value");
    assert!(
        !row.type_product().dependencies().is_empty(),
        "nominal source establishes genuine HOME TYPE custody"
    );
    let world = match kind {
        ValueTypeFixture::SharedNominal | ValueTypeFixture::HistoricalNominal => world,
        ValueTypeFixture::CapturedNominal => {
            let binder = &first
                .world_program()
                .item(0)
                .expect("actual first World item")
                .binders()[0];
            let symbol = tidepool_repr::execution_schema::SymbolIdentity {
                unit: "main".into(),
                module: binder.module().into(),
                namespace: "value".into(),
                occurrence: binder.name().into(),
                record_parent: None,
            };
            first
                .item_successor(
                    &world,
                    first.items()[0].checked_item(),
                    [
                        crate::compiler_world::CompilerWorldBinding::from_checked_value(
                            binder.name(),
                            &symbol,
                            tidepool_repr::SessionVarId::from_extract(binder.varid()),
                            0,
                            row.clone(),
                        ),
                    ],
                )
                .expect("actual completed first World successor")
                .world()
                .clone()
        }
    };
    let operand = CompilerWorldCellOperand::from_checked_host_execution(
        &world,
        WorldCellOperandExposure::Input,
        0,
        execution,
    )
    .expect("nominal operand from its checked execution");
    let (prepared, admission) = settle(|| {
        let checked = match kind {
            ValueTypeFixture::SharedNominal | ValueTypeFixture::HistoricalNominal => {
                prepare_checked(
                    &endpoint,
                    &world,
                    &source,
                    "let supplied = input",
                    Some(operand),
                    &[(second_generation, "supplied")],
                )?
            }
            ValueTypeFixture::CapturedNominal => {
                let template =
                    include_str!("../../tests/fixtures/world-native/SelectionTemplate.hs")
                        .replace("{{OPERAND_MODULE}}", &operand.symbol().module)
                        .replace("{{OPERAND_BINDER}}", &operand.symbol().occurrence);
                let plan = crate::compiler_world::plan_cell(
                    &endpoint,
                    &world,
                    Arc::new(tidepool_extract_cmd::WorldCellRecipe {
                        source: include_str!(
                            "../../tests/fixtures/world-native/WorldTypeCaptured.hs"
                        )
                        .into(),
                        template,
                        turn_templates: Vec::new(),
                    }),
                    Some(operand),
                )?;
                let origin =
                    CompilerTransactionOrigin::current().expect("actual compiler transaction");
                let subject = WorldCellAdmissionSubject::for_source_transaction(
                    &origin,
                    [17; 16],
                    0,
                    1,
                    1,
                    [b"captured declaration TYPE fixture".to_vec()],
                )?;
                let mut declarations = 0;
                let requests = plan
                    .items()
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        Ok(match item.kind() {
                            crate::cell_plan::ParsedCellPlanKind::Declaration => {
                                declarations += 1;
                                crate::cell_plan::WorldCellReservationRequest::Declaration {
                                    declaration: second_generation + 10 + index as u64,
                                }
                            }
                            crate::cell_plan::ParsedCellPlanKind::Bind => {
                                crate::cell_plan::WorldCellReservationRequest::Bind {
                                    value: second_generation,
                                    bindings: vec![(
                                        "supplied".into(),
                                        tidepool_repr::SessionVarId::from_extract(
                                            tidepool_codegen::prepared_program::session_var_id(
                                                &tidepool_repr::SessionModule::val(
                                                    tidepool_repr::Generation(second_generation),
                                                )
                                                .module_name(),
                                                "supplied",
                                            ),
                                        ),
                                    )],
                                }
                            }
                            _ => {
                                return Err(CompileError::ExtractFailed(
                                    "captured TYPE fixture has unexpected parser item".into(),
                                ))
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?;
                assert!(
                    declarations > 0,
                    "fixture must issue a real authored declaration"
                );
                let reservation = plan.reserve(plan.issue_admission_token(subject, requests)?)?;
                crate::compiler_world::admit_world_cell_program(
                    &endpoint,
                    reservation,
                    source.clone(),
                    source.clone(),
                    &origin,
                )?
                .map_err(|refusal| {
                    CompileError::ExtractFailed(format!(
                        "actual captured TYPE fixture refused: {refusal:?}"
                    ))
                })?
            }
        };
        prepare_world_cell_program(checked)
    });
    let second = prepared
        .seal(&admission)
        .expect("actual dependent value compiler END");
    let second = second
        .items()
        .last()
        .expect("actual dependent checked item")
        .native()
        .expect("actual dependent value execution")
        .value_interface_certificate()
        .expect("actual dependent checked value");
    if matches!(kind, ValueTypeFixture::CapturedNominal) {
        assert!(
            second
                .type_product()
                .dependencies()
                .contains_key(&row.type_product().leaf_binding()),
            "captured nominal declaration must retain the exact input value binding"
        );
        let first_owner = crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: row.owner().module_name(),
        };
        let declaration_owner = crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: tidepool_repr::SessionModule::lib(tidepool_repr::Generation(
                second_generation + 10,
            ))
            .module_name(),
        };
        assert!(
            second
                .type_product()
                .leaf_entry()
                .requirements
                .contains(&declaration_owner),
            "B must require the actual nominal Lib owner"
        );
        let declaration = second
            .type_product()
            .dependencies()
            .values()
            .find(|dependency| dependency.entry().descriptor.owner == declaration_owner)
            .expect("complete B product retains its exact Lib dependency");
        assert!(
            matches!(
                declaration.entry().payload,
                crate::artifact_inventory::ArtifactPayload::Canonical(_)
            ) && declaration.entry().requirements.contains(&first_owner),
            "the actual canonical Lib must require the input value interface"
        );
    }
    (row, second)
}

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
            let values = &[(51, "currentSelection"), (52, "laterSelection")];
            let checked = if used {
                prepare_checked(
                    &endpoint,
                    &world,
                    &source,
                    include_str!("../../tests/fixtures/world-native/SelectionUsed.hs"),
                    Some(operand.clone()),
                    values,
                )
            } else {
                // Availability alone supplies no lexical helper or value demand.
                prepare_checked_with_template(
                    &endpoint,
                    &world,
                    &source,
                    include_str!("../../tests/fixtures/world-native/SelectionUnused.hs").into(),
                    Some(operand.clone()),
                    values,
                    include_str!("../../tests/fixtures/world-native/CellTemplate.hs").into(),
                )
            }?;
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
            assert_physical_selection(&checked, Some(operand.as_ref()))?;
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
        let first_target = first.linked_native_target().expect("first actual target");
        let second_target = second.linked_native_target().expect("second actual target");
        let first_packages = first_target
            .imports()
            .iter()
            .filter_map(|import| match import {
                LinkedNativeTargetImport::Code { acquired, .. }
                    if matches!(acquired.code().body(), NativeCodeBody::Package(_)) =>
                {
                    Some(package_module(acquired))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            !first_packages.is_empty(),
            "World declaration item demands actual package roots"
        );
        assert!(
            first_target.acquired().len() > 0,
            "first item selects executable code"
        );
        let prefix = program.items()[1].compiler_context();
        assert!(
            prefix
                .compiler_source_support()
                .acquired_native_closures()
                .contains_owner(first_target.closure()),
            "prepared prefix retains the exact prior acquired owner independently of the second target demand"
        );
        for acquired in first_target.acquired() {
            let NativeCodeGrant::Home {
                binding, product, ..
            } = acquired.grant()
            else {
                continue;
            };
            if let Some(common) = second_target.closure().home(binding) {
                assert_eq!(common.code().id(), acquired.code().id());
                let NativeCodeGrant::Home {
                    product: second_product,
                    ..
                } = common.grant()
                else {
                    panic!("common HOME occurrence")
                };
                assert!(second_product.same_issuance(product));
            }
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

    // An explicit operand already places its TYPE in the request primary.
    // Visible host rows instead borrow TYPE only in the demanding stage. This
    // owning libtest must exercise its cfg(test) admission mutations directly;
    // runtime tests link the production toolchain library. The preparation and
    // admission callbacks below execute synchronously on this thread.
    let controls_before = crate::declaration_context::borrowed_type_admission_control_count();
    let (prepared, admission) = settle(|| {
        let visible = world.with_visible_host_bindings(
            0,
            [
                crate::compiler_world::CompilerWorldHostBinding::from_checked_execution(
                    "supplied",
                    operand.symbol(),
                    operand.session_var(),
                    1,
                    Arc::clone(&execution),
                ),
            ],
        )?;
        let plan = crate::compiler_world::plan_cell(
            &endpoint,
            visible.world(),
            Arc::new(tidepool_extract_cmd::WorldCellRecipe {
                source: "borrowedCapture :: Int\nborrowedCapture = supplied\n".into(),
                template: include_str!("../../tests/fixtures/world-native/CellTemplate.hs").into(),
                turn_templates: Vec::new(),
            }),
            None,
        )?;
        assert_eq!(plan.items().len(), 1, "one authored native capture");
        assert!(
            plan.operand().is_none(),
            "the visible value is not an explicit operand"
        );
        let origin = CompilerTransactionOrigin::current().expect("actual compiler transaction");
        let subject = WorldCellAdmissionSubject::for_source_transaction(
            &origin,
            [17; 16],
            0,
            1,
            1,
            [b"visible prior-request TYPE admission control".to_vec()],
        )?;
        let reservation = plan.reserve(plan.issue_admission_token(
            subject,
            vec![crate::cell_plan::WorldCellReservationRequest::Declaration { declaration: 61 }],
        )?)?;
        let checked = crate::compiler_world::admit_world_cell_program(
            &endpoint,
            reservation,
            source.clone(),
            source.clone(),
            &origin,
        )?
        .map_err(|refusal| {
            CompileError::ExtractFailed(format!(
                "actual visible-value declaration refused: {refusal:?}"
            ))
        })?;
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: operand.symbol().unit.clone(),
            module: operand.symbol().module.clone(),
        };
        assert!(
            checked
                .primary_context()
                .compiler_source_support()
                .implementation_entry(&owner)
                .is_none(),
            "the visible value must remain outside the request primary TYPE selection"
        );
        assert_eq!(
            checked.runtime_binding_demand().existing(),
            &[operand.session_var()],
            "the actual authored declaration demands the sealed visible value"
        );
        prepare_world_cell_program(checked)
    });
    let program = prepared
        .seal(&admission)
        .expect("actual visible-value declaration END");
    assert_eq!(program.items().len(), 1);
    assert!(program.items()[0]
        .checked_item()
        .planned_declaration()
        .is_some());
    assert!(
        crate::declaration_context::borrowed_type_admission_control_count() > controls_before,
        "the visible prior-request declaration must execute exact, omitted, and changed TYPE admission controls"
    );
}

#[test]
#[ignore = "requires the declared rooted compiler service; toolchain_world_produced_value_prefix_test owns it"]
fn real_world_produced_value_prefix_preserves_closed_indexes_without_an_operand() {
    let ((prepared, foreign_operation), admission) = settle(|| {
        let (endpoint, world, source) = open_empty_world()?;
        let checked = prepare_checked(
            &endpoint,
            &world,
            &source,
            include_str!("../../tests/fixtures/world-native/SelectionUnused.hs"),
            None,
            &[(51, "currentSelection"), (52, "laterSelection")],
        )?;
        assert_eq!(checked.stages().len(), 1);
        assert_eq!(checked.stages()[0].ordinals(), &[0, 1]);
        assert!(checked.runtime_binding_demand().existing().is_empty());
        assert!(checked.runtime_binding_demand().prior_stage().is_empty());
        assert_physical_selection(&checked, None)?;
        let operation = Arc::clone(&checked.validation().inventory);
        Ok((prepare_world_cell_program(checked)?, operation))
    });
    let program = prepared
        .seal(&admission)
        .expect("independent produced-value prefix reaches actual compiler END");
    assert_eq!(program.items().len(), 2);
    let foreign_inventory = program.items()[0]
        .compiler_context()
        .artifact_view()
        .inventory();

    let (prepared, admission) = settle(|| {
        let (endpoint, world, source) = open_empty_world()?;
        let checked = prepare_checked(
            &endpoint,
            &world,
            &source,
            "let currentSelection = (1 :: Int)\nlet laterSelection = (2 :: Int)\nlet dependentSelection = laterSelection + 1\n",
            None,
            &[
                (51, "currentSelection"),
                (52, "laterSelection"),
                (53, "dependentSelection"),
            ],
        )?;
        assert_private_future_dependency(&checked, foreign_inventory, &foreign_operation)?;
        prepare_world_cell_program(checked)
    });
    let program = prepared
        .seal(&admission)
        .expect("private future-value history reaches actual compiler END");
    assert_eq!(program.items().len(), 3);
}

fn assert_private_future_dependency(
    checked: &Arc<CheckedWorldCellProgram>,
    foreign_inventory: &crate::artifact_inventory::ArtifactInventory,
    foreign_operation: &Arc<tidepool_repr::execution_schema::InventoryOperation>,
) -> Result<(), CompileError> {
    use crate::artifact_inventory::{
        ArtifactBindingNode, ArtifactDependency, NativeRequirementRoot,
    };
    use certified_products::tests::ActualItemProofObservation;
    assert_eq!(checked.stages().len(), 1);
    assert_eq!(checked.stages()[0].ordinals(), &[0, 1, 2]);
    let mut validation =
        PackageInterfaceValidation::with_inventory(checked.validation().inventory.clone());
    let produced = crate::checked_cell::CheckedValueInputs::capture_world_produced_types(
        checked,
        &mut validation,
    )?;
    let physical = produced.for_segment(0..3)?;
    let later = physical.current_interface(52)?.artifact_id();
    let before_segment = ActualItemProofObservation::capture();
    let pending = crate::declaration_context::WorldProgramContinuation::begin(checked).open_next(
        checked,
        &physical,
        &mut validation,
        &mut |_| {},
    )?;
    ActualItemProofObservation::capture().assert_issued_after(&before_segment);
    let segment = pending.segment();
    let stage = segment.originals.captured_stage();
    let stage_packages = segment.originals.package_witnesses();
    assert!(!Arc::ptr_eq(foreign_operation, &validation.inventory));
    let mut future_edges = BTreeSet::new();
    for availability in stage.source_selection().issued_native_availability() {
        for offer in availability.offers().filter(|offer| offer.is_full()) {
            let graph = offer
                .proof()
                .expect("Full has its issued proof")
                .selection
                .graph_selection();
            for scope in &graph.selections {
                for (source, target, edge) in &scope.dependencies {
                    if let (
                        ArtifactBindingNode::Group(group),
                        ArtifactBindingNode::Artifact(value),
                        ArtifactDependency::NativeBinding {
                            generation: 52,
                            namespace,
                            occurrence,
                            record_parent: None,
                            ..
                        },
                    ) = (source, target, edge)
                    {
                        if value.artifact == later
                            && namespace == "value"
                            && occurrence == "laterSelection"
                        {
                            future_edges.insert((*group, *value));
                        }
                    }
                }
            }
        }
    }
    assert!(!future_edges.is_empty(),
        "the actual physical Full offer must retain a native body edge to the second produced VALUE");
    assert!(stage.issued_artifacts().selected_native_groups().is_empty());
    tracing::debug!(?future_edges, "actual private future VALUE dependency");

    let items = (0..3)
        .map(|index| {
            let relative = PathBuf::from(format!("item-{index}"));
            let path = checked
                .item(index)
                .expect("actual checked item")
                .executable()
                .expect("actual executable item")
                .manifest()
                .keys()
                .find(|path| {
                    path.to_str()
                        .is_some_and(|path| path.ends_with(".prepared.cbor"))
                })
                .expect("actual prepared output");
            let bytes = crate::checked_cell::world_output_bytes(checked, &relative.join(path))?;
            let artifact = crate::prepared_artifact::PreparedArtifact::parse_shared(
                Arc::new(bytes.as_ref().to_vec()),
                &crate::prepared_artifact::production_requirements()?,
                DecodeLimits::default(),
            )?;
            let receipt = certified_products::decode_segment_item_with_operation(
                crate::checked_cell::world_output_bytes(
                    checked,
                    &relative.join("certified-products.cbor"),
                )?,
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
            Ok((artifact, receipt))
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let inventory = stage.issued_artifacts().inventory();
    let mut cursor = segment
        .originals
        .bound_artifacts()
        .map_err(compiler_evidence_failure)?;
    let mut native_cursor = segment.native_prefix_artifacts()?;
    for (index, (artifact, receipt)) in items.iter().enumerate() {
        let item_types = physical.for_item(index)?;
        let mut refusal_cursor = cursor.clone();
        let before_item = ActualItemProofObservation::capture();
        let (certified, _, packages, package_selection) = segment
            .originals
            .select_item(
                &mut cursor,
                receipt.clone(),
                &artifact.bind_package_program(),
                Some(&item_types),
                &mut validation,
            )
            .map_err(compiler_evidence_failure)?;
        before_item.assert_item_reuses_segment(&segment.originals, &certified);
        assert_eq!(segment.originals.package_witnesses(), stage_packages);
        for (owner, witness) in &receipt.packages {
            assert_eq!(
                stage_packages.get(owner),
                Some(witness),
                "this actual workload observes packages already owned by its segment"
            );
        }
        assert!(
            packages.is_empty(),
            "observing an already owned package does not add item package state"
        );
        assert!(Arc::ptr_eq(certified.captured_stage(), stage));
        assert_item_prefix_keeps_closed_indexes(
            &certified,
            &crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                    51 + index as u64,
                ))
                .module_name(),
            },
        )?;
        let entry = crate::checked_cell::CheckedTypedEntry::issue_world_item(
            checked,
            index,
            artifact.prepared_shared(),
            &segment.source().generated_source_owner(),
            &certified.data().recovery_products,
        )?;
        let entry_key = entry.native_group_key();
        let values = segment
            .originals
            .selected_value_entries(&certified.data().value_interfaces)
            .cloned()
            .collect::<Vec<_>>();
        let catalog = segment_package_availability(&segment.originals, &packages);
        let before_target = inventory.metrics().issued_scope_nodes;
        let target_cursor = native_cursor.clone();
        let target = inventory.select_native_target(
            certified.physical_cohort(),
            package_selection.authenticated_imports().clone(),
            Some(entry.clone()),
            Some(&target_cursor),
            &values,
            &catalog,
            &mut validation,
        )?;
        assert_eq!(inventory.metrics().issued_scope_nodes, before_target);
        native_cursor = target.artifacts().clone();
        assert!(native_cursor.selected_native_groups().contains(&entry_key));
        if index == 0 {
            let before_refused_items = ActualItemProofObservation::capture();
            let before_refused_scopes = inventory.metrics().issued_scope_nodes;
            let before_foreign_work = foreign_operation.work_usage().unwrap();
            let foreign_item = segment.originals.select_item(
                &mut refusal_cursor,
                receipt.clone(),
                &artifact.bind_package_program(),
                Some(&item_types),
                &mut PackageInterfaceValidation::with_inventory(Arc::clone(foreign_operation)),
            );
            assert!(matches!(
                foreign_item,
                Err(certified_products::CertificationError::Mismatch(
                    "segment inventory accounting owner"
                ))
            ));
            assert_eq!(foreign_operation.work_usage().unwrap(), before_foreign_work);
            before_refused_items.assert_item_reuses_segment(&segment.originals, &certified);
            assert_eq!(
                inventory.metrics().issued_scope_nodes,
                before_refused_scopes
            );

            let (unused_owner, unused_witness) = stage_packages
                .iter()
                .find(|(owner, _)| {
                    !receipt.globals.iter().any(|global| {
                        matches!(&global.owner,
                        certified_products::ReceiptImportOwner::Package {unit, module, ..}
                        if unit == &owner.0 && module == &owner.1)
                    }) && !artifact
                        .prepared_shared()
                        .bindings()
                        .iter()
                        .flat_map(|group| match group {
                            tidepool_repr::execution_schema::Group::NonRecursive(top) => {
                                std::slice::from_ref(top)
                            }
                            tidepool_repr::execution_schema::Group::Recursive(tops) => {
                                tops.as_slice()
                            }
                        })
                        .any(|top| {
                            top.identity.unit == owner.0
                                && top.identity.module == owner.1
                                && top.identity.namespace == "value"
                        })
                })
                .expect("the actual segment owns a package unused by its first target");
            assert!(!receipt.packages.contains_key(unused_owner));
            let mut unused_package = receipt.clone();
            unused_package
                .packages
                .insert(unused_owner.clone(), unused_witness.clone());
            let unused_item = segment.originals.select_item(
                &mut refusal_cursor,
                unused_package,
                &artifact.bind_package_program(),
                Some(&item_types),
                &mut validation,
            );
            assert!(matches!(
                unused_item,
                Err(certified_products::CertificationError::Mismatch(
                    "item package has no target demand"
                ))
            ));
            before_refused_items.assert_item_reuses_segment(&segment.originals, &certified);
            assert_eq!(
                inventory.metrics().issued_scope_nodes,
                before_refused_scopes
            );
            assert_eq!(segment.originals.package_witnesses(), stage_packages);

            // Both refusals leave the same actual prefix ready for this item.
            let before_retry = ActualItemProofObservation::capture();
            let (retried, _, retry_packages, _) = segment
                .originals
                .select_item(
                    &mut refusal_cursor,
                    receipt.clone(),
                    &artifact.bind_package_program(),
                    Some(&item_types),
                    &mut validation,
                )
                .map_err(compiler_evidence_failure)?;
            before_retry.assert_item_reuses_segment(&segment.originals, &retried);
            assert!(Arc::ptr_eq(
                retried.captured_stage(),
                certified.captured_stage()
            ));
            assert_eq!(
                retried.compiler_inputs().role_projection(),
                certified.compiler_inputs().role_projection(),
            );
            assert_eq!(retry_packages, packages);

            let before_foreign = foreign_inventory.metrics().issued_scope_nodes;
            let before_original = inventory.metrics().issued_scope_nodes;
            let cursor_before_foreign = target_cursor.capture_graph_selection();
            let foreign = foreign_inventory.select_native_target(
                certified.physical_cohort(),
                package_selection.authenticated_imports().clone(),
                Some(entry),
                Some(&target_cursor),
                &values,
                &catalog,
                &mut validation,
            );
            assert!(
                matches!(foreign, Err(CompileError::ExtractFailed(ref detail))
                if detail == "artifact inventory: target belongs to another inventory")
            );
            assert_eq!(inventory.metrics().issued_scope_nodes, before_original);
            assert_eq!(
                foreign_inventory.metrics().issued_scope_nodes,
                before_foreign
            );
            assert_eq!(
                target_cursor.capture_graph_selection(),
                cursor_before_foreign
            );
            assert!(native_cursor.artifact_ids().contains(&later));
            assert!(!native_cursor.contains_namespace_artifact(later));
            let roots = native_cursor
                .selected_native_groups()
                .into_iter()
                .map(|group| NativeRequirementRoot::Group {
                    artifact: group.artifact,
                    original_ordinal: group.original_ordinal,
                })
                .collect::<Vec<_>>();
            assert!(native_cursor
                .native_binding_requirements_from_roots(&roots)?
                .iter()
                .all(|required| required.artifact_id != later));
            assert!(!certified
                .source_selection()
                .interface_only_artifact(&crate::declaration_join::ExactModuleIdentity {
                    unit: "main".into(),
                    module: tidepool_repr::SessionModule::val(tidepool_repr::Generation(52))
                        .module_name(),
                })
                .is_some());
            let (later_artifact, later_receipt) = &items[2];
            let observed = certified_products::AuthenticatedTargetImports::issue(
                later_artifact.prepared_shared(),
                &later_receipt.globals,
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?;
            let later_entry = crate::checked_cell::CheckedTypedEntry::issue_world_item(
                checked,
                2,
                later_artifact.prepared_shared(),
                &segment.source().generated_source_owner(),
                &certified.data().recovery_products,
            )?;
            let before_refusal = inventory.metrics().issued_scope_nodes;
            let refusal = inventory.select_native_target(
                certified.physical_cohort(),
                observed,
                Some(later_entry),
                Some(&native_cursor),
                &values,
                &catalog,
                &mut validation,
            );
            assert!(
                matches!(refusal, Err(CompileError::ExtractFailed(ref detail))
                if detail == "artifact inventory: target reaches a future value interface"),
                "the actual dependent target must refuse its private future VALUE: {refusal:?}"
            );
            assert_eq!(inventory.metrics().issued_scope_nodes, before_refusal);
        }
        if index == 2 {
            assert!(native_cursor.artifact_ids().contains(&later));
            assert!(native_cursor
                .binding_dependencies()
                .iter()
                .any(|(source, target, edge)| {
                    matches!((source, target, edge),
                    (ArtifactBindingNode::Group(group), ArtifactBindingNode::Artifact(value),
                        ArtifactDependency::NativeBinding { generation: 52, occurrence, .. })
                    if future_edges.contains(&(*group, *value)) && occurrence == "laterSelection")
                }));
        }
    }
    Ok(())
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
    let selected = products
        .linked_native_target()
        .expect("package item owns actual linked selection");
    assert_eq!(
        products
            .native_source_products()
            .expect("item source products resolve through item issuer")
            .target()
            .definitions()
            .globals()
            .len(),
        selected.imports().len(),
        "item target retains one selected link per physical global"
    );
    let package_roots = selected
        .imports()
        .iter()
        .filter_map(|import| match import {
            LinkedNativeTargetImport::Code { acquired, .. }
                if matches!(acquired.code().body(), NativeCodeBody::Package(_)) =>
            {
                Some(package_module(acquired))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        package_roots.len(),
        3,
        "Data.List.sort retains three ordered package roots: {package_roots:?}"
    );
    assert!(
        package_roots.iter().all(|owner| owner.ends_with("Resume")),
        "ordered roots retain issued package owners: {package_roots:?}"
    );
    let home_rows = selected
        .acquired()
        .filter(|acquired| {
            matches!(acquired.code().body(), NativeCodeBody::Home(_))
                && package_children(acquired)
                    .iter()
                    .map(package_module)
                    .eq(["GHC.Classes", "GHC.Internal.Data.OldList"])
        })
        .collect::<Vec<_>>();
    assert_eq!(home_rows.len(), 1, "one package-using HOME declaration");
    let NativeCodeGrant::Home { binding, .. } = home_rows[0].grant() else {
        panic!("actual HOME declaration")
    };
    assert_eq!(binding.original_ordinal, 0);
    let home_packages = package_children(&home_rows[0]);
    assert_eq!(home_packages.len(), 2);
    assert_eq!(package_module(&home_packages[0]), "GHC.Classes");
    assert_eq!(
        package_module(&home_packages[1]),
        "GHC.Internal.Data.OldList"
    );
    assert_eq!(home_packages[1].imports().len(), 2);
    let transitive_home_rows = selected
        .acquired()
        .filter(|acquired| {
            matches!(acquired.code().body(), NativeCodeBody::Home(_))
                && package_children(acquired)
                    .iter()
                    .map(package_module)
                    .eq(["Tidepool.Internal.Resume", "Tidepool.Internal.Resume"])
        })
        .collect::<Vec<_>>();
    assert_eq!(
        transitive_home_rows.len(),
        1,
        "transitively reached HOME row retains package obligations"
    );
    let transitive_packages = package_children(&transitive_home_rows[0]);
    assert_eq!(transitive_packages.len(), 2);
    assert_ne!(
        transitive_packages[0].occurrence(),
        transitive_packages[1].occurrence(),
        "ordered slots retain two actual cohort occurrence choices"
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
        let body = include_str!("../../tests/fixtures/world-native/PackageWorldConsumer.hs");
        let checked = prepare_checked(
            &endpoint,
            &world,
            &source,
            body,
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
            .linked_native_target()
            .expect("later item owns actual linked target")
            .imports()
            .iter()
            .all(|import| match import {
                LinkedNativeTargetImport::Code { acquired, .. }
                    if matches!(acquired.code().body(), NativeCodeBody::Package(_)) =>
                {
                    let module = package_module(acquired);
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
    let producer = test_support::compiler_input_directory().unwrap();
    let sources = producer.path().join("sources");
    std::fs::create_dir(&sources).unwrap();
    std::fs::write(
        sources.join("CachedPackageWorldHome.hs"),
        include_str!("../../tests/fixtures/world-native/CachedPackageWorldHome.hs"),
    )
    .unwrap();
    let wrapper = sources.join("CachedPackageWorldEntry.hs");
    std::fs::write(
        &wrapper,
        include_str!("../../tests/fixtures/world-native/CachedPackageWorldEntry.hs"),
    )
    .unwrap();
    let frozen = FrozenEntrySources::capture(&[sources], &wrapper).unwrap();
    let entry = test_support::prepare_frozen_production_entry(
        &frozen,
        producer.path(),
        &producer.path().join("entry"),
    )
    .expect("the cached HOME owner comes from the production entry compiler");
    let owner = crate::declaration_join::ExactModuleIdentity {
        unit: "main".into(),
        module: "CachedPackageWorldHome".into(),
    };
    let publication = entry
        .products()
        .published_source_original_selection(
            &frozen
                .source_revision(b"cached package World fixture")
                .unwrap(),
            &owner,
        )
        .expect("the actual output owns its published source selection");
    let primary = Arc::new(
        crate::declaration_context::ExactDeclarationContext::new(&[], &[], Vec::new())
            .unwrap()
            .with_published_source_originals(&publication)
            .unwrap(),
    );
    assert!(primary.source_scope().admits(&owner));
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
            &[(41, "cachedWorldValue"), (42, "cachedZeroValue")],
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
    let selected = products
        .linked_native_target()
        .expect("cached HOME owns actual linked target");
    let home_rows = selected.acquired().filter(|acquired| matches!(acquired.code().body(), NativeCodeBody::Home(body) if body.owner().module == "CachedPackageWorldHome")).collect::<Vec<_>>();
    assert!(
        !home_rows.is_empty(),
        "World target demands Original HOME code"
    );
    for acquired in &home_rows {
        let NativeCodeGrant::Home {
            binding, product, ..
        } = acquired.grant()
        else {
            panic!("actual HOME grant")
        };
        let offered = primary
            .compiler_source_support()
            .native_availability()
            .issued_row(binding.binding.artifact())
            .expect("selected group belongs to actual Original input");
        assert!(
            offered.exact_group_support(binding).is_some(),
            "target retains exact input group issuer"
        );
        let issued = products
            .target_native_inputs()
            .certified_groups()
            .get(binding)
            .expect("actual target certifier owns selected group");
        assert!(
            product.same_issuance(issued),
            "linked acquisition retains selected issuing product"
        );
    }
    let packages = home_rows
        .iter()
        .flat_map(package_children)
        .map(|acquired| package_module(&acquired).to_owned())
        .collect::<Vec<_>>();
    assert!(
        packages.iter().any(|module| module == "Data.Text.Internal"),
        "cached HOME retains Data.Text package roots: {packages:?}"
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
        let reservation = plan.reserve(
            plan.issue_admission_token(
                subject,
                slots
                    .into_iter()
                    .zip(binders)
                    .map(|(slot, bindings)| {
                        use crate::cell_plan::WorldCellReservationRequest as Request;
                        use crate::checked_cell::CheckedPlannedCellSlot as Slot;
                        match slot {
                            Slot::Prologue { declaration } => {
                                assert!(bindings.is_empty());
                                Request::Prologue { declaration }
                            }
                            Slot::Declaration { declaration } => {
                                assert!(bindings.is_empty());
                                Request::Declaration { declaration }
                            }
                            Slot::Bind { value } => Request::Bind { value, bindings },
                            Slot::Expression {
                                capture,
                                observation_name,
                            } => {
                                assert_eq!(bindings.len(), 1);
                                let binding = bindings.into_iter().next().unwrap();
                                assert_eq!(binding.0, observation_name);
                                Request::Expression { capture, binding }
                            }
                        }
                    })
                    .collect(),
            )?,
        )?;
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
        VisibleWorldBindings::from_published_bindings(0, std::iter::empty())?,
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
    let template = match operand.as_ref() {
        Some(operand) => {
            let symbol = operand.symbol();
            include_str!("../../tests/fixtures/world-native/SelectionTemplate.hs")
                .replace("{{OPERAND_MODULE}}", &symbol.module)
                .replace("{{OPERAND_BINDER}}", &symbol.occurrence)
        }
        None => include_str!("../../tests/fixtures/world-native/CellTemplate.hs").into(),
    };
    prepare_checked_with_template(
        endpoint,
        world,
        source,
        body.into(),
        operand,
        values,
        template.into(),
    )
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
    let requests = values
        .iter()
        .map(
            |(generation, name)| crate::cell_plan::WorldCellReservationRequest::Bind {
                value: *generation,
                bindings: vec![(
                    (*name).into(),
                    tidepool_repr::SessionVarId::from_extract(
                        tidepool_codegen::prepared_program::session_var_id(
                            &tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                                *generation,
                            ))
                            .module_name(),
                            name,
                        ),
                    ),
                )],
            },
        )
        .collect();
    let reservation = plan.reserve(plan.issue_admission_token(subject, requests)?)?;
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
    operand: Option<&CompilerWorldCellOperand>,
) -> Result<(), CompileError> {
    let produced_owners = [51, 52].map(|generation| crate::declaration_join::ExactModuleIdentity {
        unit: "main".into(),
        module: tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation))
            .module_name(),
    });
    for owner in &produced_owners {
        assert!(
            checked
                .primary_inputs()
                .inputs()
                .compiler_source_support()
                .compiler_roles()
                .get(owner)
                .is_none(),
            "the original request cannot supply the newly produced value owner"
        );
    }
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
    let pending = crate::declaration_context::WorldProgramContinuation::begin(checked).open_next(
        checked,
        &physical,
        &mut validation,
        &mut |_| {},
    )?;
    let segment = pending.segment();
    let input = operand.map(|operand| {
        let artifact = operand.checked_row().certified_interface().artifact_id();
        let types = operand.checked_row().type_product();
        for entry in std::iter::once(types.leaf_entry())
            .chain(types.dependencies().values().map(|row| row.entry()))
        {
            assert!(
                checked
                    .primary_inputs()
                    .inputs()
                    .compiler_source_support()
                    .compiler_roles()
                    .get(&entry.descriptor.owner)
                    .is_none(),
                "operand TYPE custody must remain borrowed by the stage"
            );
            assert_eq!(
                segment
                    .originals
                    .captured_stage()
                    .compiler_inputs()
                    .role_projection()
                    .snapshot_roles()
                    .get(&entry.descriptor.owner)
                    .map(|role| role.interface()),
                Some(entry.descriptor.id),
                "the stage borrows the operand's complete TYPE closure"
            );
        }
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: operand.symbol().unit.clone(),
            module: operand.symbol().module.clone(),
        };
        let demanded = checked
            .runtime_binding_demand()
            .existing()
            .contains(&operand.session_var());
        assert_eq!(
            segment
                .originals
                .captured_stage()
                .compiler_inputs()
                .role_projection()
                .snapshot_roles()
                .get(&owner)
                .map(|role| role.interface()),
            Some(artifact),
            "the actual stage retains the operand TYPE even without value demand"
        );
        assert_eq!(
            segment
                .originals
                .source_selection()
                .interface_only_artifact(&owner),
            Some(artifact),
        );
        assert_eq!(
            segment
                .originals
                .value_entries()
                .iter()
                .any(|entry| entry.descriptor.id == artifact),
            demanded,
            "only an actually demanded operand is emitted as a captured value"
        );
        (owner, artifact, demanded)
    });
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
    assert_item_prefix_keeps_closed_indexes(&certified, &produced_owners[0])?;
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
    if let Some((owner, artifact, demanded)) = &input {
        assert_eq!(
            certified.source_selection().interface_only_artifact(owner),
            Some(*artifact),
            "item projection preserves held operand TYPE availability"
        );
        assert_eq!(
            certified
                .data()
                .value_interfaces
                .iter()
                .any(|interface| interface.artifact_id() == *artifact),
            *demanded,
            "item output and held input availability remain distinct"
        );
    }
    assert!(certified
        .data()
        .value_interfaces
        .iter()
        .any(|interface| interface.artifact_id() == current));
    assert!(!certified
        .data()
        .value_interfaces
        .iter()
        .any(|interface| interface.artifact_id() == later));
    for (generation, expected) in [(51, Some(current)), (52, None)] {
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
    assert_item_prefix_keeps_closed_indexes(&second, &produced_owners[1])?;
    let owner = crate::declaration_join::ExactModuleIdentity {
        unit: "main".into(),
        module: tidepool_repr::SessionModule::val(tidepool_repr::Generation(52)).module_name(),
    };
    assert!(!certified
        .compiler_inputs()
        .role_projection()
        .snapshot_roles()
        .contains_key(&owner));
    assert_eq!(
        second
            .compiler_inputs()
            .role_projection()
            .snapshot_roles()
            .get(&owner)
            .map(|role| role.interface()),
        Some(later)
    );
    Ok(())
}

fn assert_item_prefix_keeps_closed_indexes(
    item: &certified_products::CapturedProducts,
    added_owner: &crate::declaration_join::ExactModuleIdentity,
) -> Result<(), CompileError> {
    let expected = item.compiler_inputs().compiler_source_support();
    assert!(expected.compiler_roles().get(added_owner).is_some());
    let closed = crate::artifact_inventory::CompilerSourceSupport::from_certified_output_fragment(
        item.source_selection(),
        item.compiler_inputs().issued_artifacts(),
    )?;
    assert!(
        closed.same_selected_acquisitions(expected),
        "closing an actual item prefix preserves its exact acquired rows"
    );
    assert_eq!(
        closed.projection().snapshot_roles(),
        expected.projection().snapshot_roles(),
    );
    assert_eq!(closed.recorded_rows(), expected.recorded_rows());
    assert!(closed
        .projection()
        .snapshot_roles()
        .contains_key(added_owner));
    assert!(closed.recorded_rows().contains_key(added_owner));
    Ok(())
}
