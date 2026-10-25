//! Read-only SOURCE metadata from an admitted primary and its exact issued contribution.

use std::collections::BTreeSet;
use std::sync::Arc;

use tidepool_repr::execution_schema::{InventoryOperation, SymbolIdentity};

use crate::artifact_inventory::{ArtifactBinding, ArtifactPayload, NativeGroupBinding};
use crate::certified_products::{
    original_native_group_definitions, original_native_group_imports,
    AuthenticatedOriginalBindings, CertificationError, ReceiptImportOwner,
};
use crate::declaration_join::ExactModuleIdentity;
use crate::CompileError;

use super::{
    ChosenNativeGroupSupport, NativeRetainedOccurrence, RetainedValueEvidence, WorldPrimaryInputs,
};

/// An observation keeps the same admitted SOURCE context, exact input selection
/// and accounting owner alive. It cannot authorize native acquisition or values.
pub(crate) struct SourceDependencyBasis {
    _primary: WorldPrimaryInputs,
    _membership: Arc<AuthenticatedOriginalBindings>,
    rows: Vec<SourceDependencyRow>,
}

pub(crate) struct SourceDependencyRow {
    reference: ReceiptImportOwner,
    support: ChosenNativeGroupSupport,
    retained: Vec<OwnedRetainedOccurrence>,
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct OwnedRetainedOccurrence {
    demander: NativeGroupBinding,
    global_index: u32,
    binding: ArtifactBinding,
    identity: SymbolIdentity,
    generation: u64,
}

fn refuse(detail: &'static str) -> CompileError {
    CompileError::CompilerEvidence(Box::new(CertificationError::Mismatch(detail)))
}

impl SourceDependencyBasis {
    pub(crate) fn observe(
        primary: &WorldPrimaryInputs,
        query: crate::certified_products::native_demand::SourceDependencyQuery,
        operation: &Arc<InventoryOperation>,
    ) -> Result<Self, CompileError> {
        let references = query.into_originals();
        for reference in &references {
            crate::host_work::checkpoint()?;
            let ReceiptImportOwner::OriginalBinding { unit, module, .. } = reference else {
                return Err(refuse(
                    "SOURCE dependency query requires exact Original uses",
                ));
            };
            operation
                .charge(unit.len().saturating_add(module.len()))
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let owner = ExactModuleIdentity {
                unit: unit.clone(),
                module: module.clone(),
            };
            if !primary.context().source_scope().admits(&owner) {
                return Err(refuse(
                    "SOURCE dependency owner is outside admitted source scope",
                ));
            }
        }
        let membership = AuthenticatedOriginalBindings::issue(
            primary.inputs().shared_compiler_source_support(),
            Arc::clone(operation),
            references.iter(),
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        operation
            .reserve::<SourceDependencyRow>(references.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut rows = Vec::with_capacity(references.len());
        for reference in references {
            crate::host_work::checkpoint()?;
            let support = membership
                .available_group(&reference, operation)?
                .ok_or_else(|| {
                    let ReceiptImportOwner::OriginalBinding { original, .. } = &reference else {
                        return refuse("authenticated SOURCE dependency reference changed kind");
                    };
                    let Some((_, ordinal)) = membership.get(&reference) else {
                        return refuse("SOURCE dependency lacks authenticated pair membership");
                    };
                    crate::artifact_inventory::admission_failure(
                    crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                        artifact: original.artifact,
                        original_ordinal: *ordinal,
                    },
                )
                })?;
            let scope = crate::artifact_inventory::SelectedOriginalGroupScope::contribution(
                support.contribution(),
            );
            let root = support.binding();
            operation
                .reserve::<NativeGroupBinding>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let mut visited = BTreeSet::new();
            let mut pending = vec![root];
            let mut retained = Vec::new();
            while let Some(demander) = pending.pop() {
                crate::host_work::checkpoint()?;
                if visited.contains(&demander) {
                    continue;
                }
                operation
                    .reserve::<(NativeGroupBinding, [usize; 4])>(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                visited.insert(demander);
                operation
                    .charge(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                let entry = scope.group_payload(demander)?;
                let ArtifactPayload::Original(product) = &entry.payload else {
                    return Err(refuse("SOURCE dependency group payload is not an Original"));
                };
                let imports = original_native_group_imports(product, demander.original_ordinal)
                    .ok_or_else(|| {
                        refuse("SOURCE dependency lacks its certified global imports")
                    })?;
                let definitions =
                    original_native_group_definitions(product, demander.original_ordinal)
                        .ok_or_else(|| {
                            refuse("SOURCE dependency lacks its certified group definitions")
                        })?;
                if definitions.globals().len() != imports.len() {
                    return Err(refuse(
                        "SOURCE dependency global slots differ from certified imports",
                    ));
                }
                for (index, import) in imports.iter().enumerate() {
                    crate::host_work::checkpoint()?;
                    operation
                        .charge(1)
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                    let ReceiptImportOwner::Retained {
                        identity,
                        generation,
                    } = import
                    else {
                        continue;
                    };
                    let global_index = u32::try_from(index).map_err(|_| {
                        refuse("SOURCE dependency global slot exceeds its wire index")
                    })?;
                    operation
                        .reserve::<OwnedRetainedOccurrence>(1)
                        .and_then(|_| {
                            operation.charge(
                                identity
                                    .unit
                                    .len()
                                    .saturating_add(identity.module.len())
                                    .saturating_add(identity.namespace.len())
                                    .saturating_add(identity.occurrence.len())
                                    .saturating_add(
                                        identity.record_parent.as_ref().map_or(0, String::len),
                                    ),
                            )
                        })
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                    let binding =
                        scope.retained_binding(demander, identity, *generation, operation)?;
                    retained.push(OwnedRetainedOccurrence {
                        demander,
                        global_index,
                        binding,
                        identity: identity.clone(),
                        generation: *generation,
                    });
                }
                let children = scope.children(demander, operation)?;
                // The stack visits children in their issued edge order.
                pending.extend(children.into_iter().rev());
            }
            retained.sort();
            rows.push(SourceDependencyRow {
                reference,
                support,
                retained,
            });
        }
        Ok(Self {
            _primary: primary.clone(),
            _membership: membership,
            rows,
        })
    }

    pub(crate) fn rows(&self) -> &[SourceDependencyRow] {
        &self.rows
    }

    /// Compare a runtime row's immutable type/evidence with one exact observed
    /// terminal. Runtime access still belongs to its independently scoped owner.
    pub(crate) fn select_retained_values_for_use(
        &self,
        reference: &ReceiptImportOwner,
        requested: &[NativeRetainedOccurrence<'_>],
    ) -> Result<Vec<RetainedValueEvidence>, CompileError> {
        let observed = self
            .rows
            .iter()
            .find(|row| row.reference() == reference)
            .ok_or_else(|| refuse("SOURCE terminal has no issued exact use"))?;
        requested
            .iter()
            .map(|occurrence| {
                let found = observed.retained.binary_search_by(|candidate| {
                    NativeRetainedOccurrence {
                        demander: candidate.demander,
                        global_index: candidate.global_index,
                        binding: &candidate.binding,
                        identity: &candidate.identity,
                        generation: candidate.generation,
                    }
                    .cmp(occurrence)
                });
                if found.is_err() {
                    return Err(refuse("SOURCE terminal exceeds its issued exact use"));
                }
                let entry = crate::artifact_inventory::SelectedOriginalGroupScope::contribution(
                    observed.support.contribution(),
                )
                .retained_payload(occurrence.demander, *occurrence.binding)?;
                Ok(RetainedValueEvidence {
                    entry: Arc::clone(entry),
                })
            })
            .collect()
    }
}

impl SourceDependencyRow {
    pub(crate) fn reference(&self) -> &ReceiptImportOwner {
        &self.reference
    }

    pub(crate) fn retained_occurrences(
        &self,
    ) -> impl ExactSizeIterator<Item = NativeRetainedOccurrence<'_>> {
        self.retained
            .iter()
            .map(|occurrence| NativeRetainedOccurrence {
                demander: occurrence.demander,
                global_index: occurrence.global_index,
                binding: &occurrence.binding,
                identity: &occurrence.identity,
                generation: occurrence.generation,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact_inventory::CompilerInputRole;

    fn source_query(
        references: Vec<ReceiptImportOwner>,
        operation: &InventoryOperation,
    ) -> crate::certified_products::native_demand::SourceDependencyQuery {
        use crate::certified_products::native_demand::DependencyQuery;
        use ciborium::value::Value;
        let binding = |binding: &ArtifactBinding| {
            Value::Array(vec![
                Value::Bytes(binding.artifact.0.to_vec()),
                Value::Bytes(binding.selection.0.to_vec()),
            ])
        };
        let uses = references
            .iter()
            .map(|reference| {
                let ReceiptImportOwner::OriginalBinding {
                    unit,
                    module,
                    interface,
                    original,
                    binder,
                } = reference
                else {
                    panic!("SOURCE fixture requests an exact Original use");
                };
                Value::Array(vec![
                    Value::Text(unit.clone()),
                    Value::Text(module.clone()),
                    binding(interface),
                    binding(original),
                    Value::Array(vec![
                        Value::Text(binder.unit.clone()),
                        Value::Text(binder.module.clone()),
                        Value::Text(binder.namespace.clone()),
                        Value::Text(binder.occurrence.clone()),
                        binder
                            .record_parent
                            .as_ref()
                            .map_or(Value::Null, |parent| Value::Text(parent.clone())),
                    ]),
                ])
            })
            .collect();
        let query = Value::Array(vec![
            Value::Text("TPDEPENDENCYQUERY1".into()),
            Value::Integer(0u64.into()),
            Value::Array(uses),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&query, &mut bytes).unwrap();
        let DependencyQuery::Source(query) = DependencyQuery::decode(&bytes, operation).unwrap()
        else {
            unreachable!()
        };
        query
    }

    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn source_observation_consumes_a_checked_world_retained_terminal() {
        use crate::cell_plan::{
            ParsedCellPlanKind, WorldCellAdmissionSubject, WorldCellReservationRequest,
        };
        use crate::compiler_world::{
            CapturedCompilerSourceRoot, CapturedCompilerSources, CompilerWorld,
            CompilerWorldBinding, CompilerWorldLexicalSurface, VisibleWorldBindings,
            WorldSourceSelection,
        };
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use tidepool_extract_cmd::{
            settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
            CompilerTransactionClose, CompilerTransactionOrigin,
        };
        let bodies = BTreeMap::from([(
            PathBuf::from("Module.hs"),
            Arc::<[u8]>::from(&b"module Module where\n"[..]),
        )]);
        let manifest = crate::cache::SourceRootManifest::from_file_digests(
            bodies
                .iter()
                .map(|(path, body)| (path.clone(), blake3::hash(body).to_hex().to_string())),
        )
        .unwrap();
        let sources = Arc::new(
            CapturedCompilerSources::from_captured_roots(
                vec![CapturedCompilerSourceRoot { manifest, bodies }],
                Arc::from(&b"SOURCE offer retained terminal"[..]),
                Arc::new(crate::artifacts::test_support::compiler_input_directory().unwrap()),
            )
            .unwrap(),
        );
        let source =
            WorldSourceSelection::new(*sources.seal(), "SOURCE offer retained terminal").unwrap();
        let compile_cell = |predecessor: Option<Arc<CompilerWorld>>,
                            text: &str,
                            generation: u64| {
            let settled = settle_compiler_transaction_cancellable(
                CompilerTransactionCancellation::new(),
                |_| {},
                || {
                    let configured = crate::toolchain::CompilerDeploymentConfiguration::from_env()
                        .expect("declared compiler deployment");
                    let (endpoint, _, _) =
                        crate::toolchain::bind_admitted_extract_endpoint(&configured)
                            .expect("configured compiler endpoint");
                    let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
                        .expect("admitted compiler endpoint");
                    let world = match predecessor {
                        Some(world) => world,
                        None => crate::compiler_world::open_world(
                            crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
                                None,
                                VisibleWorldBindings::from_published_bindings(
                                    0,
                                    std::iter::empty(),
                                )?,
                                sources.clone(),
                                CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None)?,
                            )?,
                            &endpoint,
                        )?,
                    };
                    let plan = crate::compiler_world::plan_cell(
                        &endpoint,
                        &world,
                        Arc::new(tidepool_extract_cmd::WorldCellRecipe {
                            source: text.into(),
                            // Bind desugaring uses the authored surface's pure-failure instance.
                            template: include_str!(
                                "../../tests/fixtures/world-native/CellTemplate.hs"
                            )
                            .replace(
                                "{{CELL_PRAGMAS}}",
                                "{-# LANGUAGE FlexibleInstances #-}\n{{CELL_PRAGMAS}}",
                            )
                            .replace(
                                "{{CELL_DECLS}}",
                                "instance MonadFail (Eff effects) where fail = Prelude.error\n{{CELL_DECLS}}",
                            ),
                            turn_templates: Vec::new(),
                        }),
                        None,
                    )?;
                    let origin = CompilerTransactionOrigin::current()
                        .expect("actual cell compiler operation");
                    let subject = WorldCellAdmissionSubject::for_source_transaction(
                        &origin,
                        [7; 16],
                        0,
                        generation,
                        generation,
                        [b"SOURCE exact contribution control".to_vec()],
                    )?;
                    let requests = plan
                        .items()
                        .iter()
                        .map(|item| match item.kind() {
                            ParsedCellPlanKind::Prologue => WorldCellReservationRequest::Prologue {
                                declaration: generation,
                            },
                            ParsedCellPlanKind::Declaration => {
                                WorldCellReservationRequest::Declaration {
                                    declaration: generation,
                                }
                            }
                            ParsedCellPlanKind::Bind => WorldCellReservationRequest::Bind {
                                value: generation,
                                bindings: item
                                    .binders()
                                    .iter()
                                    .map(|name| {
                                        (
                                            name.clone(),
                                            tidepool_repr::SessionVarId::from_extract(
                                                tidepool_codegen::prepared_program::session_var_id(
                                                    &tidepool_repr::SessionModule::val(
                                                        tidepool_repr::Generation(generation),
                                                    )
                                                    .module_name(),
                                                    name,
                                                ),
                                            ),
                                        )
                                    })
                                    .collect(),
                            },
                            _ => panic!("fixture uses checked binds and declarations"),
                        })
                        .collect();
                    let reservation =
                        plan.reserve(plan.issue_admission_token(subject, requests)?)?;
                    let checked = crate::compiler_world::admit_world_cell_program(
                        &endpoint,
                        reservation,
                        source.clone(),
                        source.clone(),
                        &origin,
                    )?
                    .map_err(|refusal| {
                        CompileError::ExtractFailed(format!("SOURCE fixture refused: {refusal:?}"))
                    })?;
                    crate::artifacts::prepare_world_cell_program(checked)
                },
            );
            if let Err(error) = settled.action() {
                panic!(
                    "SOURCE fixture preparation: {error:?}; close {:?}",
                    settled.close()
                );
            }
            assert!(matches!(settled.close(), CompilerTransactionClose::Clean));
            let admission = settled.host_admission_cancellation().unwrap().clone();
            settled
                .into_outcome()
                .action
                .unwrap()
                .seal(&admission)
                .unwrap()
        };
        let first = compile_cell(None, "terminal <- pure (11 :: Int)", 11);
        assert_eq!(first.items().len(), 1);
        let item = &first.items()[0];
        let binder = &first.world_program().item(0).unwrap().binders()[0];
        let value = item
            .native()
            .unwrap()
            .value_interface_certificate()
            .unwrap();
        let entry = value.type_product().leaf_entry();
        let expected_identity = SymbolIdentity {
            unit: entry.descriptor.owner.unit.clone(),
            module: binder.module().into(),
            namespace: "value".into(),
            occurrence: binder.name().into(),
            record_parent: None,
        };
        let generation = value.owner().gen.0;
        assert_eq!(binder.module(), value.owner().module_name());
        let expected_artifact = entry.descriptor.id;
        let initial = first.world_program().reservation().plan().world_owned();
        let successor = first
            .item_successor(
                initial,
                item.checked_item(),
                [CompilerWorldBinding::from_checked_value(
                    binder.name(),
                    &expected_identity,
                    tidepool_repr::SessionVarId::from_extract(binder.varid()),
                    1,
                    Arc::clone(&value),
                )],
            )
            .unwrap();
        let declaration_predecessor = successor.world().clone();
        let declaration = compile_cell(
            Some(declaration_predecessor.clone()),
            "{-# NOINLINE child #-}\nchild :: Int\nchild = terminal",
            12,
        );
        assert_eq!(
            declaration.items().len(),
            1,
            "one actual authored child item"
        );
        let declaration_item = &declaration.items()[0];
        let authored = declaration_item
            .checked_item()
            .planned_declaration()
            .unwrap();
        let child_module = authored.product().owner().module.clone();

        let context = declaration_item.compiler_context();
        let owner = ExactModuleIdentity {
            unit: authored.product().owner().unit.clone(),
            module: child_module,
        };
        assert!(context.source_scope().admits(&owner));
        let primary = context
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        let selected = primary
            .inputs()
            .compiler_source_support()
            .compiler_primary_input(&owner)
            .unwrap()
            .unwrap();
        let reference = ReceiptImportOwner::OriginalBinding {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            interface: selected.interface(),
            original: selected.original().unwrap(),
            binder: SymbolIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
                namespace: "value".into(),
                occurrence: "child".into(),
                record_parent: None,
            },
        };
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let membership = AuthenticatedOriginalBindings::issue(
            primary.inputs().shared_compiler_source_support(),
            operation.clone(),
            [&reference],
        )
        .unwrap();
        let chosen = membership
            .available_group(&reference, &operation)
            .unwrap()
            .unwrap();
        assert!(chosen.contribution().offer().is_full());
        let before = context.snapshot();
        let before_graph = context.artifact_view().capture_graph_selection();
        let before_selected = context.artifact_view().selected_native_groups();
        let before_passive_selected = membership
            .acquisition()
            .artifacts()
            .selected_native_groups();
        let passive = membership
            .acquisition()
            .artifacts()
            .capture_graph_selection();
        assert!(
            !passive.native_groups.contains(&chosen.binding()),
            "the declaration's Full offer owns a group absent from passive custody"
        );
        let observed = SourceDependencyBasis::observe(
            &primary,
            source_query(vec![reference.clone()], &operation),
            &operation,
        )
        .unwrap();
        let occurrences = observed.rows()[0]
            .retained_occurrences()
            .collect::<Vec<_>>();
        assert_eq!(
            occurrences.len(),
            1,
            "the checked declaration captures a terminal"
        );
        assert_eq!(occurrences[0].identity, &expected_identity);
        assert_eq!(occurrences[0].generation, generation);
        assert_eq!(occurrences[0].binding.artifact, expected_artifact);
        let selected = observed
            .select_retained_values_for_use(&reference, &occurrences)
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].entry.descriptor.id, expected_artifact);
        assert!(selected[0].matches_checked_value(&value).unwrap());
        let actual = occurrences[0];
        let ReceiptImportOwner::OriginalBinding {
            interface: unrelated_binding,
            ..
        } = &reference
        else {
            unreachable!()
        };
        let mut unrelated_identity = actual.identity.clone();
        unrelated_identity.occurrence.push_str("Outside");
        let mut unrelated_demander = actual.demander;
        unrelated_demander.original_ordinal += 1;
        for outside in [
            NativeRetainedOccurrence {
                global_index: actual.global_index + 1,
                ..actual
            },
            NativeRetainedOccurrence {
                generation: actual.generation + 1,
                ..actual
            },
            NativeRetainedOccurrence {
                binding: unrelated_binding,
                ..actual
            },
            NativeRetainedOccurrence {
                identity: &unrelated_identity,
                ..actual
            },
            NativeRetainedOccurrence {
                demander: unrelated_demander,
                ..actual
            },
        ] {
            let error = observed
                .select_retained_values_for_use(&reference, &[outside])
                .err()
                .expect("an altered occurrence is outside this exact SOURCE use");
            assert!(matches!(error, CompileError::CompilerEvidence(error)
            if matches!(error.as_ref(), CertificationError::Mismatch(
                "SOURCE terminal exceeds its issued exact use"
            ))));
        }
        let mut outside_reference = reference.clone();
        let ReceiptImportOwner::OriginalBinding { binder, .. } = &mut outside_reference else {
            unreachable!()
        };
        binder.occurrence.push_str("Outside");
        let error = observed
            .select_retained_values_for_use(&outside_reference, &occurrences)
            .err()
            .expect("another SOURCE use has no observed terminal authority");
        assert!(matches!(error, CompileError::CompilerEvidence(error)
        if matches!(error.as_ref(), CertificationError::Mismatch(
            "SOURCE terminal has no issued exact use"
        ))));
        assert!(observed
            .select_retained_values_for_use(&reference, &occurrences)
            .is_ok());
        let after = context.snapshot();
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            before_graph
        );
        assert_eq!(after.native_availability, before.native_availability);
        assert_eq!(after.recorded_rows(), before.recorded_rows());
        assert_eq!(
            context.artifact_view().selected_native_groups(),
            before_selected
        );
        assert_eq!(
            membership
                .acquisition()
                .artifacts()
                .selected_native_groups(),
            before_passive_selected
        );
        assert_eq!(
            membership
                .acquisition()
                .artifacts()
                .capture_graph_selection(),
            passive
        );
    }

    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn source_dependency_observer_checks_actual_scope_without_native_acquisition() {
        let fixture =
            crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
        let context = fixture
            .entry
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let primary = context
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        let owner = &fixture.root.descriptor.owner;
        let selected = context
            .compiler_source_support()
            .compiler_primary_input(owner)
            .unwrap()
            .unwrap();
        assert!(matches!(
            context.compiler_input_projection().snapshot_roles()[owner],
            CompilerInputRole::ReusableOriginal { .. }
        ));
        let ArtifactPayload::Original(product) = &fixture.root.payload else {
            unreachable!()
        };
        let binder = original_native_group_definitions(product, fixture.root_ordinals[1])
            .unwrap()
            .export_contracts()
            .map(Result::unwrap)
            .find(|contract| contract.identity.occurrence == "second")
            .unwrap()
            .identity
            .clone();
        let reference = ReceiptImportOwner::OriginalBinding {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            interface: selected.interface(),
            original: selected.original().unwrap(),
            binder,
        };
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let before = context.snapshot();
        let before_graph = context.artifact_view().capture_graph_selection();
        let observed = SourceDependencyBasis::observe(
            &primary,
            source_query(vec![reference.clone()], &operation),
            &operation,
        )
        .unwrap();
        assert_eq!(
            observed.rows().len(),
            1,
            "the observed row retains the exact decoded SOURCE use"
        );
        assert!(observed
            .rows()
            .iter()
            .all(|row| row.reference() == &reference));
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            before_graph,
            "metadata observation does not change selected native roots"
        );

        let without_source = context
            .as_ref()
            .clone()
            .rebuild(|facts| {
                facts.source_scope = Default::default();
                facts.lexical = Default::default();
            })
            .finish()
            .unwrap();
        let without_source = without_source
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        let member = AuthenticatedOriginalBindings::issue(
            without_source.inputs().shared_compiler_source_support(),
            operation.clone(),
            [&reference],
        )
        .unwrap();
        assert!(
            member.get(&reference).is_some(),
            "exact pair membership alone is not SOURCE admission"
        );
        let error = SourceDependencyBasis::observe(
            &without_source,
            source_query(vec![reference], &operation),
            &operation,
        )
        .err()
        .unwrap();
        assert!(
            matches!(error, CompileError::CompilerEvidence(error) if matches!(error.as_ref(), CertificationError::Mismatch("SOURCE dependency owner is outside admitted source scope")))
        );
        let after = context.snapshot();
        assert_eq!(after.native_availability, before.native_availability);
        assert_eq!(after.recorded_rows(), before.recorded_rows());
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            before_graph
        );
    }
    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn acquired_group_scope_checks_explicit_membership_without_union_ambiguity() {
        use crate::artifact_inventory::{
            ArtifactInventory, NativeArtifactDemand, NativeGroupKey, SelectedOriginalGroupScope,
        };
        let fixture =
            crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
        let context = fixture
            .entry
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let inventory = ArtifactInventory::default();
        let base = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                context.artifact_view().entries(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let primary = context
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        let selected = context
            .compiler_source_support()
            .compiler_primary_input(&fixture.root.descriptor.owner)
            .unwrap()
            .unwrap();
        let ArtifactPayload::Original(product) = &fixture.root.payload else {
            unreachable!()
        };
        let issue = |ordinals: &[u32]| {
            let mut complete = BTreeSet::new();
            for ordinal in ordinals {
                let binder = original_native_group_definitions(product, *ordinal)
                    .unwrap()
                    .export_contracts()
                    .map(Result::unwrap)
                    .find(|contract| {
                        contract.identity.module == fixture.root.descriptor.owner.module
                    })
                    .unwrap()
                    .identity
                    .clone();
                let reference = ReceiptImportOwner::OriginalBinding {
                    unit: fixture.root.descriptor.owner.unit.clone(),
                    module: fixture.root.descriptor.owner.module.clone(),
                    interface: selected.interface(),
                    original: selected.original().unwrap(),
                    binder,
                };
                let membership = AuthenticatedOriginalBindings::issue(
                    primary.inputs().shared_compiler_source_support(),
                    operation.clone(),
                    [&reference],
                )
                .unwrap();
                let chosen = membership
                    .available_group(&reference, &operation)
                    .unwrap()
                    .unwrap();
                let issued = SelectedOriginalGroupScope::contribution(chosen.contribution());
                let mut pending = vec![chosen.binding()];
                while let Some(group) = pending.pop() {
                    if complete.insert(NativeGroupKey {
                        artifact: group.binding.artifact(),
                        original_ordinal: group.original_ordinal,
                    }) {
                        pending.extend(issued.children(group, &operation).unwrap());
                    }
                }
            }
            inventory
                .admit_recovery_selection(&base, vec![], &complete)
                .unwrap()
        };
        let left = issue(&fixture.root_ordinals[..1]);
        let right = issue(&fixture.root_ordinals);
        let group = |view: &crate::artifact_inventory::ArtifactView| {
            *SelectedOriginalGroupScope::selected(view)
                .bindings(&operation)
                .unwrap()
                .iter()
                .find(|group| {
                    group.binding.artifact() == fixture.root.descriptor.id
                        && group.original_ordinal == fixture.root_ordinals[0]
                })
                .unwrap()
        };
        let left_group = group(&left);
        let right_group = group(&right);
        assert_ne!(
            left_group, right_group,
            "control holds independent group issuers"
        );
        let explicit = BTreeSet::from([left_group]);
        let missing = SelectedOriginalGroupScope::acquired(&base, &explicit, &operation)
            .err()
            .unwrap();
        assert!(
            matches!(missing, CompileError::ArtifactInventory(error) if matches!(error.failure,
            crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal }
                if artifact == left_group.binding.artifact() && original_ordinal == left_group.original_ordinal)),
            "payload custody alone cannot authenticate a group"
        );
        let both = left.retain_custody(&right).unwrap();
        let before = both.capture_graph_selection();
        let scope = SelectedOriginalGroupScope::acquired(&both, &explicit, &operation).unwrap();
        assert!(scope.group_payload(left_group).is_ok());
        assert!(scope.children(left_group, &operation).is_ok());
        assert!(scope.group_payload(right_group).is_err());
        assert!(scope.children(right_group, &operation).is_err());
        assert!(
            SelectedOriginalGroupScope::selected(&both)
                .group_payload(right_group)
                .is_err(),
            "a selected query cannot use a custody-only group"
        );
        let right_scope = SelectedOriginalGroupScope::selected(&right);
        let parent = right_scope
            .bindings(&operation)
            .unwrap()
            .into_iter()
            .find(|group| {
                group.binding.artifact() == fixture.root.descriptor.id
                    && group.original_ordinal == fixture.root_ordinals[1]
            })
            .unwrap();
        assert!(
            !right_scope.children(parent, &operation).unwrap().is_empty(),
            "the incomplete-set refusal has a real exact child"
        );
        let incomplete = BTreeSet::from([parent]);
        assert!(
            SelectedOriginalGroupScope::acquired(&both, &incomplete, &operation)
                .unwrap()
                .children(parent, &operation)
                .is_err(),
            "acquired traversal requires complete explicit children"
        );
        assert_eq!(scope.bindings(&operation).unwrap(), vec![left_group]);
        assert_eq!(both.capture_graph_selection(), before);
    }
    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn source_observation_keeps_the_selected_historical_body_with_newer_custody() {
        let fixture =
            crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
        let original_context = fixture
            .entry
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let ArtifactPayload::Original(old_product) = &fixture.root.payload else {
            unreachable!()
        };
        let source_path = fixture.includes[0].join("SourceOriginalSupport.hs");
        let source = std::fs::read_to_string(&source_path).unwrap();
        assert!(source.contains("first = 17"));
        std::fs::write(&source_path, source.replace("first = 17", "first = 19")).unwrap();
        let frozen = crate::artifacts::FrozenEntrySources::capture(
            &fixture.includes,
            &fixture.includes[0].join("PublicationTarget.hs"),
        )
        .unwrap();
        let directory = fixture.includes[0].parent().unwrap();
        let newer = crate::artifacts::test_support::prepare_frozen_production_entry(
            &frozen,
            directory,
            &directory.join("newer-source"),
        )
        .unwrap();
        let newer_context = newer
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let old_input = original_context
            .compiler_source_support()
            .compiler_primary_input(&fixture.root.descriptor.owner)
            .unwrap()
            .unwrap();
        let new_input = newer_context
            .compiler_source_support()
            .compiler_primary_input(&fixture.root.descriptor.owner)
            .unwrap()
            .unwrap();
        assert_ne!(
            old_input.original(),
            new_input.original(),
            "the second actual compile must issue another native body"
        );
        let ArtifactPayload::Original(new_product) = &new_input.entry().payload else {
            unreachable!()
        };
        assert_ne!(old_product.owner(), new_product.owner());
        let binder = original_native_group_definitions(old_product, fixture.root_ordinals[0])
            .unwrap()
            .export_contracts()
            .map(Result::unwrap)
            .find(|contract| contract.identity.occurrence == "first")
            .unwrap()
            .identity
            .clone();
        let reference = ReceiptImportOwner::OriginalBinding {
            unit: binder.unit.clone(),
            module: binder.module.clone(),
            interface: old_input.interface(),
            original: old_input.original().unwrap(),
            binder: binder.clone(),
        };
        let newer_reference = ReceiptImportOwner::OriginalBinding {
            unit: binder.unit.clone(),
            module: binder.module.clone(),
            interface: new_input.interface(),
            original: new_input.original().unwrap(),
            binder,
        };
        let historical = original_context
            .as_ref()
            .clone()
            .rebuild(|facts| {
                facts.compiler_support = facts
                    .compiler_support
                    .retain_custody(newer_context.artifact_view())
                    .unwrap();
            })
            .finish()
            .unwrap();
        assert!(historical
            .artifact_view()
            .observed_payload(&new_input.original().unwrap().artifact)
            .is_some());
        let primary = historical
            .capture_world_primary_inputs(historical.toolchain_identity_sha256())
            .unwrap();
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let observed = SourceDependencyBasis::observe(
            &primary,
            source_query(vec![reference.clone()], &operation),
            &operation,
        )
        .unwrap();
        assert_eq!(
            &observed._membership.get(&reference).unwrap().0,
            old_product.owner()
        );
        assert!(
            SourceDependencyBasis::observe(
                &primary,
                source_query(vec![newer_reference], &operation),
                &operation
            )
            .is_err(),
            "newer byte custody cannot replace the selected exact source body"
        );
        assert!(
            SourceDependencyBasis::observe(
                &primary,
                source_query(vec![reference], &operation),
                &operation
            )
            .is_ok(),
            "refusal preserves the historical basis"
        );
    }
}
