//! Source-boot fixture delivery through the existing certification owners.
//! This adapter is compiled only into the owning crate's test executable.

mod codec;

use super::*;
use crate::certified_products::{
    certify_products, decode_receipt_in, CapturedProducts, CertifiedProducts, ParsedModuleProducts,
};
use crate::declaration_context::ExactDeclarationContext;
use crate::declaration_join::ExactModuleIdentity;

#[derive(Clone, Copy)]
enum PacketProducer {
    OriginalProducts,
    AuthoredDeclaration,
    WorldValueType,
    Codec,
}

impl PacketProducer {
    fn label(self) -> &'static str {
        match self {
            Self::OriginalProducts => "original-products",
            Self::AuthoredDeclaration => "authored-declaration",
            Self::WorldValueType => "world-value-type",
            Self::Codec => "codec",
        }
    }
}

// Completion belongs to the request bytes actually consumed by this producer.
// It is issued last, once, after the specific output publication succeeds.
struct PacketCompletion<'a> {
    packet: &'a Path,
    producer: PacketProducer,
    request_sha: [u8; 32],
}

impl<'a> PacketCompletion<'a> {
    fn new(packet: &'a Path, producer: PacketProducer, request: &[u8]) -> Self {
        assert!(packet.is_absolute());
        assert!(request.len() <= MANIFEST_LIMIT);
        assert!(
            !packet.join("completion.cbor").exists(),
            "packet already completed"
        );
        Self {
            packet,
            producer,
            request_sha: Sha256::digest(request).into(),
        }
    }

    fn publish(self, outputs: &[PathBuf]) {
        use std::io::{Read, Write};
        assert!(
            !outputs.is_empty() && outputs.len() <= 16,
            "completed packet requires outputs"
        );
        let mut seen = BTreeSet::new();
        let mut total = 0;
        let rows = outputs
            .iter()
            .map(|path| {
                assert!(seen.insert(path.clone()), "duplicate completion output");
                assert!(path.is_absolute() && path.starts_with(self.packet));
                assert!(
                    path.canonicalize()
                        .unwrap()
                        .starts_with(self.packet.canonicalize().unwrap()),
                    "completion output escaped its packet"
                );
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(
                    &mut fs::File::open(path)
                        .unwrap()
                        .take((16 * MANIFEST_LIMIT + 1) as u64),
                    &mut bytes,
                )
                .unwrap();
                total += bytes.len();
                assert!(
                    total <= 16 * MANIFEST_LIMIT,
                    "completion output aggregate bound"
                );
                Value::Array(vec![
                    Value::Text(path.to_str().unwrap().into()),
                    Value::Integer(bytes.len().into()),
                    Value::Bytes(Sha256::digest(&bytes).to_vec()),
                ])
            })
            .collect();
        let receipt = Value::Array(vec![
            Value::Text("TPFIXTURECOMPLETE1".into()),
            Value::Text(self.producer.label().into()),
            Value::Text(self.packet.to_str().unwrap().into()),
            Value::Bytes(self.request_sha.to_vec()),
            Value::Array(rows),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&receipt, &mut bytes).unwrap();
        assert!(bytes.len() <= 8192, "completion metadata bound");
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.packet.join("completion.cbor"))
            .expect("one-shot packet completion");
        output.write_all(&bytes).unwrap();
    }
}

fn text_field(value: &Value) -> &str {
    value.as_text().expect("fixture text field")
}

fn names(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("fixture list")
        .iter()
        .map(|value| text_field(value).to_owned())
        .collect()
}

enum FixtureIntent {
    Ordinary,
    Published {
        target: ExactModuleIdentity,
        binding: String,
        roots: Vec<ExactModuleIdentity>,
    },
}

fn fixture_intent(fields: &[Value]) -> FixtureIntent {
    match fields.first().map(text_field) {
        Some("TPSOURCEBOOTFIXTURE4") => {
            assert_eq!(fields.len(), 10, "ordinary fixture request tuple");
            FixtureIntent::Ordinary
        }
        Some("TPSOURCEBOOTPUBLISHED2") => {
            assert_eq!(fields.len(), 13, "published fixture request tuple");
            assert!(
                fields[1].as_text().is_some_and(|path| Path::new(path).is_absolute())
                    && names(&fields[2]).iter().all(|path| Path::new(path).is_absolute())
                    && matches!(fields[9], Value::Null)
                    && fields[6].as_bool() == Some(true),
                "published fixture requires original source inputs and scope delivery without a capture"
            );
            assert!(
                [3, 4, 5, 8].into_iter().all(|index| names(&fields[index]).is_empty()),
                "published fixture cannot combine candidate, interface, native or lexical selections"
            );
            let target = text_field(&fields[10]);
            let roots = names(&fields[11]);
            let binding = text_field(&fields[12]);
            assert_eq!(
                binding, "__prepared",
                "published fixture requires the production entry target"
            );
            assert!(
                !target.is_empty()
                    && !roots.is_empty()
                    && roots.iter().all(|root| !root.is_empty())
                    && roots.iter().collect::<BTreeSet<_>>().len() == roots.len(),
                "published fixture requires a target and unique nonempty publication roots"
            );
            FixtureIntent::Published {
                target: ExactModuleIdentity {
                    unit: "main".into(),
                    module: target.into(),
                },
                binding: binding.into(),
                roots: roots
                    .into_iter()
                    .map(|module| ExactModuleIdentity {
                        unit: "main".into(),
                        module,
                    })
                    .collect(),
            }
        }
        _ => panic!("unknown source-boot fixture request tag"),
    }
}

fn fixture_source_imports(
    certified: &CertifiedProducts,
) -> BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>> {
    certified
        .module_interfaces
        .iter()
        .filter_map(|interface| {
            let edges = interface.source_imports()?;
            Some((
                ExactModuleIdentity {
                    unit: interface.unit().to_owned(),
                    module: interface.module().to_owned(),
                },
                edges
                    .iter()
                    .filter_map(|edge| {
                        Some(ExactModuleIdentity {
                            unit: edge.home_unit.as_ref()?.clone(),
                            module: edge.module.clone(),
                        })
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            ))
        })
        .collect()
}

// The complete certified view owns interface closure and original import
// evidence. Selecting lexical visibility never selects a native implementation.
fn admit_fixture_scope(
    producer_sha: [u8; 32],
    captured: &CapturedProducts,
    exact_owners: &[ExactModuleIdentity],
    native_owners: &[ExactModuleIdentity],
    lexical_roots: &[ExactModuleIdentity],
) -> ExactDeclarationContext {
    let certified = captured.data();
    let mut context = empty_fixture_scope(producer_sha);
    // Admit the complete interface closure before selecting any native
    // owner. Type-only dependencies cannot be inferred as packages.
    let output = captured.compiler_inputs();
    let view = output.issued_artifacts();
    let support =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(output).unwrap();
    if !exact_owners.is_empty() {
        context = context
            .extend_issued_compiler_support(
                &support
                    .selected_interfaces()
                    .unwrap()
                    .support_for_owners(exact_owners.to_vec())
                    .unwrap(),
            )
            .unwrap();
    }
    if !lexical_roots.is_empty() {
        // Canonical interfaces retain authenticated source imports even
        // when a source-only owner has no executable recovery graph.
        // Native ownership and implementation roles remain separate.
        let imports = fixture_source_imports(certified);
        let surface = crate::declaration_join::source_lexical_closure(
            lexical_roots,
            &imports,
            &[],
            &view.retained_source_implementations(),
        )
        .unwrap();
        let lexical_owners = surface
            .lexical
            .iter()
            .map(|node| node.owner.clone())
            .collect::<Vec<_>>();
        context = context
            .extend_issued_compiler_support(
                &support
                    .selected_interfaces()
                    .unwrap()
                    .support_for_owners(lexical_owners)
                    .unwrap(),
            )
            .unwrap();
        context = context.extend(&[], &[], surface.lexical).unwrap();
    }
    if !native_owners.is_empty() {
        context = context
            .extend_issued_compiler_support(
                &support
                    .selected_interfaces()
                    .unwrap()
                    .support_for_owners(native_owners.to_vec())
                    .unwrap(),
            )
            .unwrap();
        let native = certified
            .recovery_products
            .iter()
            .filter(|product| {
                native_owners.iter().any(|owner| {
                    owner.unit == product.owner().unit && owner.module == product.owner().module
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            native
                .iter()
                .map(|product| ExactModuleIdentity {
                    unit: product.owner().unit.clone(),
                    module: product.owner().module.clone(),
                })
                .collect::<BTreeSet<_>>(),
            native_owners.iter().cloned().collect::<BTreeSet<_>>(),
            "native scope owners require genuine original products"
        );
        for product in &native {
            let canonical = product.module_interface();
            assert!(
                canonical.core_bytes().is_some(),
                "native scope requires its captured canonical interface and Core"
            );
            crate::certified_products::validate_original_module_interface(product, canonical)
                .expect("native custody must match its certified canonical module");
        }
        let requested = native_owners.iter().cloned().collect::<BTreeSet<_>>();
        let selected = support.source_support(&requested).unwrap();
        assert_eq!(
            selected
                .projection()
                .snapshot_roles()
                .iter()
                .filter_map(|(owner, role)| role.original().map(|_| owner.clone()))
                .collect::<BTreeSet<_>>(),
            requested
        );
        for product in &native {
            let owner = ExactModuleIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
            };
            let original = crate::artifact_inventory::ArtifactEntry::original_artifact_id(
                producer_sha,
                product,
            );
            assert_eq!(
                selected.projection().snapshot_roles()[&owner].original(),
                Some(original)
            );
            assert_eq!(
                selected.native_availability().snapshot_facts().unwrap()[&original].mode,
                crate::declaration_context::NativeTransportMode::Full
            );
            let row = selected
                .native_availability()
                .rows()
                .find(|row| row.original_owner() == product.owner())
                .unwrap();
            for group in crate::certified_products::original_available_groups(product).unwrap() {
                assert!(row.permits(group.original_ordinal()));
            }
        }
        let selected_ids = native
            .iter()
            .map(|product| {
                crate::artifact_inventory::ArtifactEntry::original_artifact_id(
                    producer_sha,
                    product,
                )
            })
            .collect::<BTreeSet<_>>();
        for offer in selected
            .native_offers()
            .iter()
            .filter(|offer| selected_ids.contains(&offer.original()))
        {
            for group in offer.issued_group_carriers().unwrap().into_keys() {
                let row = selected
                    .native_availability()
                    .rows()
                    .find(|row| {
                        row.target_original(row.original_owner())
                            .unwrap()
                            .descriptor
                            .id
                            == group.binding.artifact()
                    })
                    .expect("actual selected output retains each required native child permission");
                assert!(row.permits(group.original_ordinal));
            }
        }
        assert!(selected.artifacts().selected_native_groups().is_empty());
        context = context.extend_issued_compiler_support(&selected).unwrap();
        assert!(context.artifact_view().selected_native_groups().is_empty());
    }
    context
}

// Structural data from the same already-issued context used for scope delivery.
// Materialization preserves its actual typed keys, proof refs and native facts;
// the parcel itself grants no compiler or execution authority.
fn materialize_fixture_context(context: &ExactDeclarationContext, packet: &Path) -> Vec<u8> {
    let encoded = crate::compiler_world::world_context_fixture(context, packet).unwrap();
    assert!(
        encoded.len() <= MANIFEST_LIMIT,
        "world context parcel bound"
    );
    encoded
}

fn empty_fixture_scope(producer_sha: [u8; 32]) -> ExactDeclarationContext {
    ExactDeclarationContext::from_authenticated_interfaces(
        producer_sha,
        &crate::artifact_inventory::ArtifactInventory::default().empty_view(),
        crate::artifact_inventory::CompilerInputProjection::default(),
    )
    .unwrap()
}

// Publication consumes actual native product rows. Canonical source-only
// interfaces belong to exact scope delivery and cannot become native records.
fn publish_fixture_candidates(
    producer: &[u8],
    include: &[PathBuf],
    evidence: &DependencyEvidence,
    parsed: ParsedModuleProducts,
    source: &str,
    certified: &CertifiedProducts,
    requested: &BTreeSet<String>,
    delivery: &Path,
) -> PathBuf {
    let native_owners = certified
        .recovery_products
        .iter()
        .map(|product| product.owner().module.clone())
        .collect::<BTreeSet<_>>();
    assert!(
        requested.is_subset(&native_owners),
        "requested candidates must have genuine native products; source-only owners require canonical scope delivery"
    );
    let (_, publication) = prepare_publication(
        producer,
        include,
        evidence,
        parsed,
        source,
        CandidateVersionOrigin::Ordinary,
        &certified.recovery_products,
    );
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let records = publication
        .cohort
        .into_records()
        .into_iter()
        .filter(|record| requested.contains(&record.module))
        .map(|mut record| {
            let canonical = record
                .module_interface_proof
                .as_ref()
                .expect("publication requires the genuinely issued canonical interface");
            // Publication normally creates this descriptor before a
            // durable record becomes selectable. Keep the fixture's
            // real proof and descriptor together in its own delivery.
            record.data.module_interface = Some(
                crate::recovery_artifacts::materialize_module_interface(
                    delivery,
                    canonical,
                    &mut validation,
                    crate::recovery_artifacts::MaterializationMode::Scratch,
                )
                .expect("genuine publication canonical descriptor materialization"),
            );
            (record, CandidateOrigin::Ordinary)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        records
            .iter()
            .map(|(record, _)| record.module.clone())
            .collect::<BTreeSet<_>>(),
        *requested,
        "genuine publication omitted a requested owner before candidate selection"
    );
    let selected = select_records_inner_with_transport(
        producer,
        include,
        delivery,
        records,
        None,
        CandidateInputTransport::DurableFiles,
    )
    .unwrap_or_else(|| {
        panic!("production candidate delivery refused requested owners {requested:?}")
    });
    assert_eq!(
        selected
            .by_owner
            .keys()
            .map(|(_, module)| module.clone())
            .collect::<BTreeSet<_>>(),
        *requested,
        "fixture must not silently decline a requested native owner or its interface closure"
    );
    let destination = delivery.join("module-candidates.cbor");
    assert_eq!(selected.manifest_path, destination);
    destination
}

fn write_fixture_scope_output(packet: &Path, manifest: &Path) {
    let result = Value::Array(vec![
        Value::Text("TPSOURCEBOOTDELIVERY1".into()),
        Value::Text(manifest.to_str().expect("UTF-8 fixture scope path").into()),
    ]);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&result, &mut bytes).unwrap();
    fs::write(packet.join("delivery.cbor"), bytes).unwrap();
}

// TempDir::keep transfers cleanup to the Haskell scoped packet owner. Dropping
// it here would invalidate absolute paths in the delivered scope before use.
fn fresh_fixture_request_root(packet: &Path) -> PathBuf {
    tempfile::Builder::new()
        .prefix("compilation-request-")
        .tempdir_in(packet)
        .expect("fresh fixture compilation request")
        .keep()
}

#[test]
#[ignore = "requires a live matched source-boot finalization packet"]
fn source_boot_candidate_packet_producer() {
    let packet = PathBuf::from(
        std::env::var_os("TIDEPOOL_CANDIDATE_FIXTURE_PACKET").expect("source-boot packet"),
    );
    assert!(packet.is_absolute());
    let request = fs::read(packet.join("request.cbor")).unwrap();
    assert!(request.len() <= MANIFEST_LIMIT);
    let completion = PacketCompletion::new(&packet, PacketProducer::OriginalProducts, &request);
    let mut outputs = Vec::new();
    let request: Value = ciborium::de::from_reader(request.as_slice()).unwrap();
    let fields = request.as_array().expect("fixture request tuple");
    let intent = fixture_intent(fields);
    let (endpoint, _) = crate::toolchain::bind_extract_endpoint().unwrap();
    let producer = endpoint.identity().producer_bytes();
    assert_eq!(text_field(&fields[7]), endpoint.identity().producer_hex());
    let producer_sha =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
            .sha256();
    let include = names(&fields[2])
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let requested = names(&fields[3]).into_iter().collect::<BTreeSet<_>>();
    let exact_owners = names(&fields[4])
        .into_iter()
        .map(|module| ExactModuleIdentity {
            unit: "main".into(),
            module,
        })
        .collect::<Vec<_>>();
    let native_owners = names(&fields[5])
        .into_iter()
        .map(|module| ExactModuleIdentity {
            unit: "main".into(),
            module,
        })
        .collect::<Vec<_>>();
    let lexical_roots = names(&fields[8])
        .into_iter()
        .map(|module| ExactModuleIdentity {
            unit: "main".into(),
            module,
        })
        .collect::<Vec<_>>();
    assert!(
        (exact_owners.is_empty() && native_owners.is_empty() && lexical_roots.is_empty())
            || fields[6].as_bool().expect("fixture scope selection"),
        "retained interface/native custody requires an actual delivered scope"
    );
    let context = if let FixtureIntent::Published {
        target,
        binding,
        roots,
    } = &intent
    {
        let source_path = PathBuf::from(text_field(&fields[1]));
        let sources = crate::artifacts::FrozenEntrySources::capture(&include, &source_path)
            .expect("published fixture requires its actual original source inputs");
        assert_eq!(binding, "__prepared");
        let entry = crate::artifacts::test_support::prepare_frozen_production_entry(
            &sources,
            &packet,
            &packet.join("published-entry"),
        )
        .expect("the production entry owner performs the publication fixture's sole compile");
        let execution = entry
            .products()
            .original_execution_evidence()
            .expect("the actual entry retains its issued completion");
        assert_eq!(
            execution.completed().unwrap().target(),
            target,
            "published target must be the actual completed source owner"
        );
        let revision = hex(&Sha256::digest(entry.source().as_bytes()));
        let mut published = empty_fixture_scope(producer_sha);
        for root in roots {
            let issued = entry
                .products()
                .published_source_original_selection(&revision, root)
                .expect("publication root must be a genuine direct import of the completed target");
            published = published
                .with_published_source_originals(&issued)
                .expect("actual published fixture selections must compose");
        }
        published
    } else if !matches!(fields[1], Value::Null) {
        let source_path = PathBuf::from(text_field(&fields[1]));
        assert!(source_path.is_absolute());
        let capture = PathBuf::from(text_field(&fields[9]));
        assert!(capture.is_absolute());
        // Source identity stays at its original authored path. The capture
        // supplies only the original bytes and payloads emitted at that path.
        let source = fs::read_to_string(capture.join("original-source.hs")).unwrap();
        let evidence_bytes = fs::read(capture.join("dependencies.json")).unwrap();
        let receipt_bytes = fs::read(capture.join("certified-products.cbor")).unwrap();
        let receipt = decode_receipt_in(&receipt_bytes, Some(&capture)).unwrap();
        assert!(
            receipt.modules.iter().all(|module| {
                module.origin == crate::certified_products::ProductOrigin::Fresh
            }) && matches!(
                &receipt.source_recipe,
                crate::certified_products::WorkerExecutionSource::Ordinary
            ),
            "fixture delivery selections cannot replace an inherited compiler request context; capture a cold original"
        );
        let product_bytes = fs::read(capture.join("module-products.cbor")).unwrap();
        let package_bytes = fs::read(capture.join("module-package-imports.cbor")).unwrap();
        let parsed = ParsedModuleProducts::decode(&product_bytes, &package_bytes).unwrap();
        let operation = std::sync::Arc::clone(parsed.operation());
        let source_admission = crate::artifacts::admit_compiler_source(
            &operation,
            &evidence_bytes,
            &source_path,
            &source,
        )
        .expect("actual consumed source and resolution evidence");
        let evidence = source_admission.completed();
        let pending = certify_products(
            None,
            &receipt,
            &parsed,
            &source_admission,
            &capture,
            producer,
            &include,
            None,
        )
        .unwrap();
        let captured = crate::declaration_context::issue_certified_product_native_availability(
            producer_sha,
            pending,
            None,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(
                std::sync::Arc::clone(&operation),
            ),
        )
        .expect("actual captured products require physical cohort issuance");
        let certified = captured.data();
        if !evidence.cache_safe || !evidence.selection_complete {
            assert!(
                certified
                    .recovery_products
                    .iter()
                    .all(|product| { product.execution_source().is_none() }),
                "completed originals cannot acquire an ordinary source replay recipe"
            );
        }
        let context = admit_fixture_scope(
            producer_sha,
            &captured,
            &exact_owners,
            &native_owners,
            &lexical_roots,
        );
        if !requested.is_empty() {
            let manifest = publish_fixture_candidates(
                producer,
                &include,
                &evidence,
                parsed,
                &source,
                certified,
                &requested,
                packet.parent().unwrap(),
            );
            // Candidate graph descriptors retain their publication directory.
            // Deliver a packet-owned snapshot of that actual manifest; Haskell
            // never treats a preexisting shared manifest as completed output.
            let snapshot = packet.join("module-candidates.cbor");
            fs::write(&snapshot, fs::read(manifest).unwrap()).unwrap();
            outputs.push(snapshot);
        }
        context
    } else {
        assert!(
            requested.is_empty()
                && exact_owners.is_empty()
                && native_owners.is_empty()
                && lexical_roots.is_empty()
                && include.is_empty()
                && matches!(fields[9], Value::Null)
        );
        empty_fixture_scope(producer_sha)
    };
    if fields[6].as_bool().expect("fixture scope selection") {
        let world_context = packet.join("world-context-blocks.cbor");
        tidepool_atomic_write::write_best_effort(
            &world_context,
            &materialize_fixture_context(&context, &packet),
        )
        .unwrap();
        outputs.push(world_context);
        // The packet receives custody of the complete issued resource before
        // this adapter exits, and releases it after Haskell consumption.
        let request_root = fresh_fixture_request_root(&packet);
        let scope = Arc::new(context)
            .prepare_fixture_compilation(&request_root, producer)
            .unwrap();
        write_fixture_scope_output(&packet, &scope.manifest);
        outputs.extend([scope.manifest.clone(), packet.join("delivery.cbor")]);
    }
    completion.publish(&outputs);
    println!(
        "genuine fixture: Rust certification, full ArtifactView admission and production delivery passed"
    );
}

#[test]
#[ignore = "requires a live matched authored-declaration fixture"]
fn source_boot_authored_declaration_packet_producer() {
    let packet = PathBuf::from(
        std::env::var_os("TIDEPOOL_CANDIDATE_FIXTURE_PACKET").expect("authored fixture packet"),
    );
    assert!(packet.is_absolute());
    let request = fs::read(packet.join("request.cbor")).unwrap();
    assert!(request.len() <= MANIFEST_LIMIT);
    let completion = PacketCompletion::new(&packet, PacketProducer::AuthoredDeclaration, &request);
    let request: Value = ciborium::de::from_reader(request.as_slice()).unwrap();
    let fields = request.as_array().expect("authored fixture request tuple");
    assert_eq!(fields.len(), 5);
    assert_eq!(text_field(&fields[0]), "TPSOURCEBOOTAUTHORED2");
    let generation: u64 = fields[1]
        .as_integer()
        .expect("reserved generation")
        .try_into()
        .unwrap();
    assert!(generation > 0);
    let include = names(&fields[2])
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    assert!(!include.is_empty() && include.iter().all(|path| path.is_absolute()));
    let source_path = PathBuf::from(text_field(&fields[3]));
    let delivery_root = PathBuf::from(text_field(&fields[4]));
    assert!(source_path.is_absolute() && delivery_root.is_absolute());
    assert_eq!(packet.parent().unwrap(), delivery_root);
    let source = fs::read_to_string(&source_path).unwrap();
    let module = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation));
    let (endpoint, _) = crate::toolchain::bind_extract_endpoint().unwrap();
    assert_eq!(
        std::env::var("TIDEPOOL_COMPILER_PRODUCER").expect("protected matched fixture producer"),
        endpoint.identity().producer_hex()
    );
    let certificate = crate::artifacts::test_support::certify_authored_declaration(
        module,
        &source_path,
        &source,
        &include,
        &include[0],
    )
    .expect("actual reserved original declaration certification");
    let context = ExactDeclarationContext::new(&[Arc::new(certificate)], &[], vec![]).unwrap();
    let owner = ExactModuleIdentity {
        unit: "main".into(),
        module: module.module_name(),
    };
    let expected_interface = context
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .get(&owner)
        .expect("issued authored canonical interface")
        .descriptor
        .id;
    let projected = context
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![owner.clone()])
        .unwrap();
    let type_context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_issued_compiler_support(&projected)
        .unwrap();
    let type_support = type_context.compiler_source_support();
    let roles = type_support.projection().snapshot_roles();
    assert_eq!(
        roles.get(&owner),
        Some(
            &crate::artifact_inventory::CompilerInputRole::InterfaceOnly {
                interface: expected_interface,
            }
        ),
    );
    assert!(roles.values().all(|role| matches!(
        role,
        crate::artifact_inventory::CompilerInputRole::InterfaceOnly { .. }
    )));
    assert!(type_support.native_offers().is_empty());
    assert!(type_support.artifacts().selected_native_groups().is_empty());
    assert!(type_context
        .module_interfaces()
        .iter()
        .any(|interface| interface.unit() == owner.unit
            && interface.module() == owner.module
            && interface.origin()
                == crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration {
                    generation
                }));
    let request_root = fresh_fixture_request_root(&packet);
    let scope = Arc::new(context)
        .prepare_fixture_compilation(&request_root, endpoint.identity().producer_bytes())
        .expect("production authored scope delivery with original carrier association");
    write_fixture_scope_output(&packet, &scope.manifest);
    completion.publish(&[scope.manifest.clone(), packet.join("delivery.cbor")]);
    println!("genuine authored fixture: reserved declaration certification, origin-preserving type-only projection and production scope delivery passed");
}

#[test]
#[ignore = "requires the declared matched World value compiler fixture"]
fn source_boot_world_value_type_packet_producer() {
    let packet = PathBuf::from(
        std::env::var_os("TIDEPOOL_CANDIDATE_FIXTURE_PACKET").expect("World value fixture packet"),
    );
    assert!(packet.is_absolute());
    let request = fs::read(packet.join("request.cbor")).unwrap();
    let completion = PacketCompletion::new(&packet, PacketProducer::WorldValueType, &request);
    let fields: Value = ciborium::de::from_reader(request.as_slice()).unwrap();
    let fields = fields.as_array().expect("World value fixture request tuple");
    assert_eq!(fields.len(), 1);
    assert_eq!(text_field(&fields[0]), "TPSOURCEBOOTVALUETYPE1");
    let bytes = crate::compiler_world::world_value_transport_fixture(&packet.join("world-values"))
        .expect("production checked-value issuance and World transport");
    let output = packet.join("world-value-types.cbor");
    tidepool_atomic_write::write_best_effort(&output, &bytes).unwrap();
    completion.publish(&[output]);
}

#[test]
fn published_fixture_intent_refuses_malformed_or_combined_selections() {
    let ordinary = vec![
        Value::Text("TPSOURCEBOOTFIXTURE4".into()),
        Value::Null,
        Value::Array(vec![]),
        Value::Array(vec![]),
        Value::Array(vec![]),
        Value::Array(vec![]),
        Value::Bool(false),
        Value::Text("producer".into()),
        Value::Array(vec![]),
        Value::Null,
    ];
    assert!(matches!(fixture_intent(&ordinary), FixtureIntent::Ordinary));
    let mut published = ordinary.clone();
    published[0] = Value::Text("TPSOURCEBOOTPUBLISHED2".into());
    published[1] = Value::Text("/fixture/source.hs".into());
    published[2] = Value::Array(vec![Value::Text("/fixture".into())]);
    published[6] = Value::Bool(true);
    published.extend([
        Value::Text("CompletedTarget".into()),
        Value::Array(vec![Value::Text("PublishedRoot".into())]),
        Value::Text("__prepared".into()),
    ]);
    assert!(
        matches!(fixture_intent(&published), FixtureIntent::Published { target, binding, roots }
        if target == (ExactModuleIdentity { unit: "main".into(), module: "CompletedTarget".into() })
        && binding == "__prepared"
        && roots == vec![ExactModuleIdentity { unit: "main".into(), module: "PublishedRoot".into() }])
    );
    let mut mutations = Vec::new();
    let mut unknown = ordinary.clone();
    unknown[0] = Value::Text("unknown".into());
    mutations.push(unknown);
    mutations.push(published[..10].to_vec());
    let mut ordinary_extra = published.clone();
    ordinary_extra[0] = ordinary[0].clone();
    mutations.push(ordinary_extra);
    let mut legacy = published.clone();
    legacy[0] = Value::Text("TPSOURCEBOOTPUBLISHED1".into());
    mutations.push(legacy);
    mutations.push(published[..12].to_vec());
    for index in [1, 10, 12] {
        let mut missing = published.clone();
        missing[index] = Value::Null;
        mutations.push(missing);
    }
    let mut captured = published.clone();
    captured[9] = Value::Text("/fixture/capture".into());
    mutations.push(captured);
    let mut relative_source = published.clone();
    relative_source[1] = Value::Text("source.hs".into());
    mutations.push(relative_source);
    let mut relative_include = published.clone();
    relative_include[2] = Value::Array(vec![Value::Text("relative".into())]);
    mutations.push(relative_include);
    for binding in ["", "answer"] {
        let mut unsupported_target = published.clone();
        unsupported_target[12] = Value::Text(binding.into());
        mutations.push(unsupported_target);
    }
    let mut no_scope = published.clone();
    no_scope[6] = Value::Bool(false);
    mutations.push(no_scope);
    for index in [3, 4, 5, 8] {
        let mut combined = published.clone();
        combined[index] = Value::Array(vec![Value::Text("other selection".into())]);
        mutations.push(combined);
    }
    let mut no_target = published.clone();
    no_target[10] = Value::Text(String::new());
    mutations.push(no_target);
    for roots in [
        vec![],
        vec![Value::Text(String::new())],
        vec![
            Value::Text("PublishedRoot".into()),
            Value::Text("PublishedRoot".into()),
        ],
    ] {
        let mut invalid = published.clone();
        invalid[11] = Value::Array(roots);
        mutations.push(invalid);
    }
    for fields in mutations {
        assert!(std::panic::catch_unwind(|| fixture_intent(&fields)).is_err());
    }
}

#[test]
fn packet_completion_refuses_partial_and_competing_issuance() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("output.cbor");
    let receipt = root.path().join("completion.cbor");
    let request = b"actual consumed request";
    let attempt = |outputs: &[PathBuf]| {
        std::panic::catch_unwind(|| {
            PacketCompletion::new(root.path(), PacketProducer::Codec, request).publish(outputs);
        })
    };
    assert!(attempt(&[]).is_err());
    assert!(!receipt.exists());
    assert!(attempt(std::slice::from_ref(&output)).is_err());
    assert!(!receipt.exists());
    let outside = tempfile::NamedTempFile::new().unwrap();
    assert!(attempt(&[outside.path().to_owned()]).is_err());
    assert!(!receipt.exists());
    fs::write(&output, b"published output").unwrap();
    let pending = PacketCompletion::new(root.path(), PacketProducer::Codec, request);
    fs::write(&receipt, b"competing one-shot completion").unwrap();
    assert!(std::panic::catch_unwind(|| pending.publish(&[output])).is_err());
    assert_eq!(fs::read(receipt).unwrap(), b"competing one-shot completion");
}
