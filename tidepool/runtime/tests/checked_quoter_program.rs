//! Reexported quotation compiles across checked slots, and the fixture-owned
//! runtime original preserves its durable source recipe before native execution.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::compiler_test_support::OwnedEnvironment;

use tidepool_repr::SessionId;
use tidepool_runtime::session::turn::{
    consume_cell_program_item, finish_cell_program_admitted, finish_world_cell_plan_admitted,
    prepare_cell_program_admitted, prepare_world_cell_plan_admitted, TemplateSelector, TurnResult,
};
use tidepool_runtime::session::{
    resident_cell_check_template, resident_workbench_templates, ModuleEnv, ResidentSession,
    SessionLib, SessionRunContext, SourceImports,
};
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_toolchain::CompileError;

fn capture_quoter_sources(
    includes: &[std::path::PathBuf],
    effects: &TestEffectSurface,
) -> (
    tidepool_runtime::session::GenerationArtifactReference,
    Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>,
) {
    use std::io::Read;
    use tidepool_toolchain::compiler_world::{CapturedCompilerSourceRoot, CapturedCompilerSources};
    let owner = Arc::new(tempfile::tempdir().unwrap());
    let mut remaining = CapturedCompilerSources::capture_byte_allowance().unwrap();
    let mut roots = Vec::new();
    for (ordinal, include) in includes.iter().enumerate() {
        remaining = remaining
            .checked_sub(4)
            .expect("quoter source root framing fits capture allowance");
        let files = tidepool_toolchain::cache::source_root_manifest(include).unwrap();
        let destination = owner.path().join(format!("root-{ordinal}"));
        std::fs::create_dir_all(&destination).unwrap();
        let mut bodies = std::collections::BTreeMap::new();
        for (relative, digest) in &files {
            let input = include.join(relative);
            let length = std::fs::metadata(&input).unwrap().len();
            let framed = 8u64
                .checked_add(relative.as_os_str().len() as u64)
                .and_then(|size| size.checked_add(length))
                .unwrap();
            remaining = remaining
                .checked_sub(framed)
                .expect("quoter source bytes fit capture allowance before allocation");
            let mut body = Vec::new();
            std::fs::File::open(&input)
                .unwrap()
                .take(length.checked_add(1).unwrap())
                .read_to_end(&mut body)
                .unwrap();
            assert_eq!(
                body.len() as u64,
                length,
                "quoter source changed during capture"
            );
            assert_eq!(
                blake3::hash(&body).to_hex().as_str(),
                digest,
                "quoter source differs from its observed manifest"
            );
            let target = destination.join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, &body).unwrap();
            bodies.insert(relative.clone(), Arc::<[u8]>::from(body));
        }
        roots.push(CapturedCompilerSourceRoot {
            manifest: tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)
                .unwrap(),
            bodies,
        });
    }
    let manifests = roots
        .iter()
        .map(|root| root.manifest.clone())
        .collect::<Vec<_>>();
    let identity = tidepool_toolchain::cache::source_manifests_identity(
        b"checked-quoter-world-source",
        &manifests,
    );
    let reference = tidepool_runtime::session::GenerationArtifactReference::capture(
        serde_json::to_vec(&(effects.preamble(), effects.row(), identity)).unwrap(),
    );
    let captured =
        CapturedCompilerSources::from_captured_roots(roots, Arc::from(reference.bytes()), owner)
            .unwrap();
    (reference, Arc::new(captured))
}

#[test]
#[serial_test::serial]
fn fresh_checked_program_retains_reexported_quoter_across_slots_cold_and_warm(
) -> Result<(), tidepool_runtime::session::SessionError> {
    tidepool_testing::eval_harness::require_extract();
    let cache = tempfile::tempdir().unwrap();
    let _cache = OwnedEnvironment::set("TIDEPOOL_COMPILE_CACHE_DIR", Some(cache.path()));
    let root = tidepool_toolchain::artifacts::compiler_scratch_directory().unwrap();
    std::fs::write(
        root.path().join("CheckedQuoterFacade.hs"),
        include_str!("fixtures/CheckedQuoterFacade.hs"),
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let lib = SessionLib::open(
        SessionId(1088),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        1024 * 1024,
        Some(lib),
    );
    let source_owner = session.mint_notebook_scope_with_lease(None).unwrap();
    let public = source_owner.scope();
    session
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let mut includes = effects.include_paths().to_vec();
    includes.insert(0, root.path().to_path_buf());
    let (reference, sources) = capture_quoter_sources(&includes, &effects);
    let admitted = session
        .capture_inspection_world_in(public, &reference, source_owner.original_source())
        .unwrap();
    let settled = source_owner
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, _| {
                tidepool_runtime::session::run_admitted_compiler_world(owner, admitted, || {
                    Ok(Some(sources))
                })
            },
        )
        .unwrap();
    let completed = tidepool_runtime::session::finish_admitted_compiler_world(settled).unwrap();
    assert!(session.install_compiler_world_if_current(
        &completed,
        &reference,
        source_owner.original_source()
    ));
    let source = include_str!("fixtures/checked-quoter-program.hs");
    for phase in ["first request", "repeated request"] {
        let settled = session
            .with_source_compiler_transaction(
                source_owner.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| session.begin_ephemeral_private_execution(public),
            )
            .unwrap();
        assert!(settled.close().resources_retired());
        let execution = Arc::new(settled.into_outcome().action?);
        let private = execution.private_scope();
        let view = session.compile_view_for_execution(&execution)?;
        let imports = view.turn_imports(&SourceImports::from_specs([
            "qualified CheckedQuoterFacade as Q",
        ]));
        let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
        let specification = Arc::new(tidepool_extract_cmd::WorldCellRecipe {
            source: source.to_string(),
            template,
            turn_templates: templates
                .iter()
                .map(|template| {
                    let selector = match template.kind {
                        TemplateSelector::Decl => "decl",
                        TemplateSelector::Bind => "bind",
                        TemplateSelector::BindDiscard => "binddiscard",
                        TemplateSelector::Expr => "expr",
                    };
                    (selector.into(), template.source.clone())
                })
                .collect(),
        });
        let settled = session
            .with_source_compiler_transaction(
                source_owner.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| session.prepare_private_execution(execution.clone(), None),
            )
            .unwrap();
        assert!(settled.close().resources_retired());
        let (snapshot, operand) = settled.into_outcome().action.unwrap().into_parts();
        assert!(operand.is_none());
        let settled = source_owner
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, _| {
                    prepare_world_cell_plan_admitted(owner, snapshot.world(), specification, None)
                },
            )
            .unwrap();
        let plan = finish_world_cell_plan_admitted(settled).unwrap();
        let settled = session
            .with_source_compiler_transaction(
                source_owner.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| {
                    session.admit_world_planned_cell_with_snapshot(execution, snapshot, plan, None)
                },
            )
            .unwrap();
        assert!(settled.close().resources_retired());
        let admission = settled.into_outcome().action.unwrap();
        let started = std::time::Instant::now();
        let settled = source_owner
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, report| prepare_cell_program_admitted(owner, admission.clone(), report),
            )
            .unwrap();
        let (checked, program) = finish_cell_program_admitted(settled)
            .unwrap_or_else(|error| panic!("{phase} checked quoter program failed: {error:?}"));
        assert_eq!(program.items().len(), 3);
        assert_eq!(
            (0..3)
                .map(|index| checked.checked_item(index).unwrap().kind())
                .collect::<Vec<_>>(),
            vec![
                tidepool_toolchain::checked_cell::CheckedItemKind::Expression,
                tidepool_toolchain::checked_cell::CheckedItemKind::Bind,
                tidepool_toolchain::checked_cell::CheckedItemKind::Expression
            ]
        );
        assert_eq!(
            checked.checked_item(0).unwrap().source().trim(),
            "42 :: Int"
        );
        for item in program.items() {
            assert!(item.native().is_some());
            assert!(item.native_products().is_some());
        }
        let immutable_program = program.program().clone();
        let first = &immutable_program.items()[0];
        let observation = first
            .native()
            .unwrap()
            .observation_name()
            .expect("expression owns its issued observation capture")
            .to_owned();
        let settled = session
            .with_source_compiler_transaction(
                source_owner.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| {
                    let prefix = session
                        .begin_cell_program(program)
                        .unwrap()
                        .expect("checked program starts with its first expression");
                    session.admit_checked_item(prefix, checked.checked_item(0).unwrap())
                },
            )
            .unwrap();
        assert!(settled.close().resources_retired());
        let reservation = settled.into_outcome().action.unwrap();
        let prefix = reservation.prefix().clone();
        assert_eq!(reservation.item().index(), 0);
        assert_eq!(reservation.observation_name(), Some(observation.as_str()));
        // Authored expressions execute through their compiler-issued singleton
        // capture; the sealed execution receipt is a Bind observation.
        let TurnResult::Bind {
            compiled: _compiled_first,
            binders,
            bound,
            ..
        } = consume_cell_program_item(reservation).unwrap()
        else {
            panic!("the authored expression must retain its issued Bind observation");
        };
        assert_eq!(binders, vec![observation.clone()]);
        assert_eq!(
            bound
                .iter()
                .map(|binder| binder.name.as_str())
                .collect::<Vec<_>>(),
            vec![observation.as_str()]
        );
        // Quotation expands during compilation. Only its own native item
        // supplies the facade original retained for durable recovery.
        let quoted_item = &immutable_program.items()[1];
        assert_eq!(quoted_item.checked_item().index(), 1);
        let quoted_products = quoted_item.native_products().unwrap();
        let products = &quoted_products.recovery_products;
        let originals = products
            .iter()
            .filter(|product| {
                product.owner().unit == "main" && product.owner().module == "CheckedQuoterFacade"
            })
            .collect::<Vec<_>>();
        assert_eq!(originals.len(), 1);
        let original = originals[0];
        assert!(quoted_products.certified_groups.iter().any(|group| {
            group.owner() == original.owner()
                && group.group().binders().iter().any(|binder| {
                    binder.unit == "main"
                        && binder.module == "CheckedQuoterFacade"
                        && binder.occurrence == "describe"
                })
        }));
        assert!(quoted_item
            .native()
            .unwrap()
            .target_owned()
            .globals()
            .iter()
            .any(|global| {
                global.identity.unit == "main"
                    && global.identity.module == "CheckedQuoterFacade"
                    && global.identity.occurrence == "describe"
            }));
        let durable = tidepool_toolchain::artifacts::compiler_scratch_directory().unwrap();
        let tidepool_toolchain::checked_cell::CellProgramBasis::World(output) =
            immutable_program.basis()
        else {
            panic!("runtime quoter preparation must retain its actual World producer");
        };
        let producer = *output.producer_sha256();
        let references = tidepool_toolchain::recovery_artifacts::materialize_certified_products(
            durable.path(),
            producer,
            products,
        )
        .unwrap();
        // A native original's canonical companion is not its interface closure:
        // imported type-only modules can have no retained native product.
        let expected_owners = products
            .iter()
            .map(|product| (product.owner().unit.clone(), product.owner().module.clone()))
            .collect::<BTreeSet<_>>();
        let companions = references
            .iter()
            .map(|reference| {
                reference
                    .module_interface
                    .clone()
                    .expect("retained original owns its canonical companion")
            })
            .collect::<Vec<_>>();
        let actual_owners = companions
            .iter()
            .map(|reference| {
                (
                    reference.interface.unit.clone(),
                    reference.interface.module.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(actual_owners, expected_owners);
        assert_eq!(
            companions.len(),
            expected_owners.len(),
            "one canonical companion per fixture owner"
        );
        let original_context = quoted_products.original_execution_context().unwrap();
        let module_interfaces = original_context
            .materialize_module_interfaces(durable.path())
            .unwrap();
        let interface_owners = module_interfaces
            .iter()
            .map(|reference| {
                (
                    reference.interface.unit.clone(),
                    reference.interface.module.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert!(expected_owners.is_subset(&interface_owners));
        let type_only_owners = interface_owners
            .difference(&expected_owners)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(
            !type_only_owners.is_empty(),
            "this fixture must exercise canonical dependencies without native products"
        );
        let selection = original_context.snapshot().graph_selection().unwrap();
        use tidepool_toolchain::declaration_join::RecoveryPayloadReference;
        let incomplete_payloads = references
            .iter()
            .cloned()
            .map(RecoveryPayloadReference::Original)
            .chain(
                companions
                    .iter()
                    .cloned()
                    .map(RecoveryPayloadReference::Canonical),
            )
            .collect::<Vec<_>>();
        let incomplete_ids = references
            .iter()
            .map(tidepool_toolchain::artifact_inventory::ArtifactDescriptor::from_recovery_product)
            .chain(companions.iter().map(
                tidepool_toolchain::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface,
            ))
            .map(|descriptor| descriptor.id)
            .collect::<BTreeSet<_>>();
        let descriptors = original_context.artifact_view().descriptors();
        let omitted = descriptors
            .iter()
            .filter(|descriptor| !incomplete_ids.contains(&descriptor.id))
            .collect::<Vec<_>>();
        assert!(!omitted.is_empty());
        for descriptor in omitted {
            assert!(type_only_owners.contains(&(
                descriptor.owner.unit.clone(),
                descriptor.owner.module.clone(),
            )));
            assert!(selection
                .bindings
                .iter()
                .any(|binding| binding.artifact == descriptor.id));
        }
        let incomplete_descriptors = descriptors
            .into_iter()
            .filter(|descriptor| incomplete_ids.contains(&descriptor.id))
            .collect::<Vec<_>>();
        let incomplete =
            tidepool_toolchain::declaration_join::RecoveredArtifactAcquisition::new(durable.path())
                .capture_payloads_bound(
                    &incomplete_payloads,
                    &incomplete_descriptors,
                    &selection.bindings,
                    &original_context.artifact_view().binding_dependencies(),
                );
        match incomplete {
            Err(tidepool_toolchain::declaration_join::RecoveryInventoryError::Certification(
                CompileError::ExtractFailed(detail),
            )) => assert_eq!(
                detail,
                "exact declaration context: bound recovery has duplicate or missing exact endpoints"
            ),
            Err(other) => panic!("expected incomplete recovery closure refusal, got {other:?}"),
            Ok(_) => panic!("incomplete canonical closure cannot authorize exact acquisition"),
        }
        assert_original_durability(
            durable.path(),
            producer,
            original,
            &references,
            &original_context,
            SourceReplay::Unavailable,
        );
        eprintln!(
            "checked quoter {phase}: {} ms",
            started.elapsed().as_millis()
        );
        // Preparation must retain authority without creating authored bindings
        // or executing the quoted command's contents.
        assert_eq!(session.scope_binding_count(public), 0);
        assert_eq!(session.scope_binding_count(private), 0);
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SourceReplay {
    Available,
    Unavailable,
}

fn assert_original_durability(
    durable: &std::path::Path,
    producer: [u8; 32],
    original: &tidepool_toolchain::recovery_artifacts::CertifiedRecoveryProduct,
    references: &[tidepool_toolchain::recovery_artifacts::RecoveryArtifactRef],
    issued: &tidepool_toolchain::declaration_join::ExactDeclarationContext,
    replay: SourceReplay,
) {
    let is_owner = |unit: &str, module: &str| {
        unit == original.owner().unit && module == original.owner().module
    };
    let matching = references
        .iter()
        .filter(|reference| is_owner(&reference.unit, &reference.module))
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1);
    let reference = matching[0];
    assert!(original.source_sha256().is_some());
    match replay {
        SourceReplay::Available => assert!(reference.execution_source.is_some()),
        SourceReplay::Unavailable => assert!(reference.execution_source.is_none()),
    }
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
    use tidepool_toolchain::artifact_inventory::ArtifactDescriptor;
    use tidepool_toolchain::declaration_join::{
        RecoveredArtifactAcquisition, RecoveryPayloadReference,
    };
    use tidepool_toolchain::exact_context::{hydrate_context, Materializer};
    let snapshot = issued.snapshot();
    let blocks = MemoryBlockstore::new();
    let mut writer = Materializer::new(&blocks);
    let persisted = writer.materialize_context(None, &snapshot).unwrap();
    for (cid, bytes) in writer.finish().unwrap().into_blocks() {
        blocks.put_keyed(&cid, &bytes).unwrap();
    }
    let acquired = hydrate_context(&blocks, persisted.root()).unwrap();
    let graph = acquired.snapshot().graph_selection().unwrap();
    let mut payload_ids = graph
        .bindings
        .iter()
        .map(|binding| binding.artifact)
        .collect::<BTreeSet<_>>();
    payload_ids.extend(snapshot.native_package_artifacts_introduced(None));
    let payloads = issued
        .materialize_recovery_artifacts(durable, &payload_ids)
        .unwrap();
    let descriptors = payloads
        .iter()
        .map(|reference| match reference {
            RecoveryPayloadReference::Original(reference) => {
                ArtifactDescriptor::from_recovery_product(reference)
            }
            RecoveryPayloadReference::Canonical(reference) => {
                ArtifactDescriptor::from_recovery_module_interface(reference)
            }
            RecoveryPayloadReference::Join(reference) => {
                ArtifactDescriptor::from_recovery_join(reference)
            }
            RecoveryPayloadReference::Value(reference) => {
                ArtifactDescriptor::from_recovery_value_interface(reference)
            }
            RecoveryPayloadReference::PackageCode(reference) => {
                ArtifactDescriptor::from_recovery_package_code(reference)
            }
        })
        .collect::<Vec<_>>();
    let recovered = RecoveredArtifactAcquisition::new(durable)
        .capture_payloads_bound(
            &payloads,
            &descriptors,
            &graph.bindings,
            &acquired.snapshot().binding_dependencies().unwrap(),
        )
        .unwrap()
        .context_for_expected_snapshot(persisted.root(), &acquired)
        .unwrap();
    assert_eq!(
        recovered.artifact_view().capture_graph_selection(),
        issued.artifact_view().capture_graph_selection()
    );
    assert_eq!(
        recovered.compiler_input_roles(),
        issued.compiler_input_roles()
    );
    assert_eq!(recovered.lexical_graph(), issued.lexical_graph());
    let recovered_products = recovered.recovery_products();
    let matching = recovered_products
        .iter()
        .filter(|product| is_owner(&product.owner().unit, &product.owner().module))
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1);
    let recovered_original = matching[0];
    // Durable recovery preserves the original, not its fresh source admission.
    assert!(recovered_original.source_sha256().is_none());
    assert_eq!(original.owner(), recovered_original.owner());
    assert_eq!(
        original.interface_bytes(),
        recovered_original.interface_bytes()
    );
    assert_eq!(original.product_bytes(), recovered_original.product_bytes());
    let recovered_durable = tidepool_toolchain::artifacts::compiler_scratch_directory().unwrap();
    let recovered_references =
        tidepool_toolchain::recovery_artifacts::materialize_certified_products(
            recovered_durable.path(),
            producer,
            &recovered_products,
        )
        .unwrap();
    let matching = recovered_references
        .iter()
        .filter(|reference| is_owner(&reference.unit, &reference.module))
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1);
    let recovered_reference = matching[0];
    assert_eq!(reference, recovered_reference);
    if let SourceReplay::Available = replay {
        let graph = &reference.execution_source.as_ref().unwrap().path;
        assert_eq!(
            std::fs::read(durable.join(graph)).unwrap(),
            std::fs::read(recovered_durable.path().join(graph)).unwrap()
        );
    }
}

#[test]
#[serial_test::serial]
fn fixture_original_source_recipe_survives_recovery_without_compile_time_execution() {
    tidepool_testing::eval_harness::require_extract();
    let cache = tempfile::tempdir().unwrap();
    let _cache = OwnedEnvironment::set("TIDEPOOL_COMPILE_CACHE_DIR", Some(cache.path()));
    let root = tidepool_toolchain::artifacts::compiler_scratch_directory().unwrap();
    std::fs::write(
        root.path().join("CheckedOriginalReplayOwner.hs"),
        include_str!("fixtures/CheckedOriginalReplayOwner.hs"),
    )
    .unwrap();
    let include = [root.path().to_path_buf()];
    let probe = root.path().join("CheckedOriginalReplayProbe.hs");
    std::fs::write(
        &probe,
        include_str!("fixtures/CheckedOriginalReplayProbe.hs"),
    )
    .unwrap();
    let frozen =
        tidepool_toolchain::artifacts::FrozenEntrySources::capture(&include, &probe).unwrap();
    let entry = tidepool_testing::with_settlement(|settlement| {
        tidepool_toolchain::artifacts::prepare_frozen_production_entry(
            &frozen,
            root.path(),
            &root.path().join("entry"),
            settlement,
        )
    })
    .unwrap();
    let compiled = entry.products();
    let issued = compiled.original_execution_context().unwrap();
    let originals = compiled
        .recovery_products
        .iter()
        .filter(|product| {
            product.owner().unit == "main" && product.owner().module == "CheckedOriginalReplayOwner"
        })
        .collect::<Vec<_>>();
    assert_eq!(originals.len(), 1);
    let original = originals[0];
    assert!(compiled.certified_groups.iter().any(|group| {
        group.owner() == original.owner()
            && group.origin() == tidepool_toolchain::certified_products::ProductOrigin::Fresh
            && group.group().binders().iter().any(|binder| {
                binder.unit == "main"
                    && binder.module == "CheckedOriginalReplayOwner"
                    && binder.occurrence == "value"
            })
    }));
    let descriptors = compiled.artifact_view().descriptors();
    let matching = descriptors
        .iter()
        .filter(|descriptor| {
            descriptor.kind == tidepool_toolchain::artifact_inventory::ArtifactKind::OriginalModule
                && descriptor.owner.unit == original.owner().unit
                && descriptor.owner.module == original.owner().module
                && descriptor.interface_sha256 == original.owner().skinny_iface_sha256
                && descriptor.product_sha256 == Some(original.owner().product_sha256)
        })
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1);
    let producer = matching[0].producer_sha256;
    let durable = tidepool_toolchain::artifacts::compiler_scratch_directory().unwrap();
    let references = tidepool_toolchain::recovery_artifacts::materialize_certified_products(
        durable.path(),
        producer,
        &compiled.recovery_products,
    )
    .unwrap();
    assert_original_durability(
        durable.path(),
        producer,
        original,
        &references,
        &issued,
        SourceReplay::Available,
    );
}
