//! Source-boot fixture delivery through the existing certification owners.
//! This adapter is compiled only into the owning crate's test executable.

mod codec;

use super::*;
use crate::certified_products::{
    certify_products, decode_receipt_in, CertifiedProducts, ParsedModuleProducts,
};
use crate::declaration_context::{certified_product_artifact_view, ExactDeclarationContext};
use crate::declaration_join::ExactModuleIdentity;

#[derive(Clone, Copy)]
enum PacketProducer {
    OriginalProducts,
    AuthoredDeclaration,
    Codec,
}

impl PacketProducer {
    fn label(self) -> &'static str {
        match self {
            Self::OriginalProducts => "original-products",
            Self::AuthoredDeclaration => "authored-declaration",
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

// The complete certified view owns interface closure and original import
// evidence. Selecting lexical visibility never selects a native implementation.
fn admit_fixture_scope(
    producer_sha: [u8; 32],
    certified: &CertifiedProducts,
    exact_owners: &[ExactModuleIdentity],
    native_owners: &[ExactModuleIdentity],
    lexical_roots: &[ExactModuleIdentity],
) -> ExactDeclarationContext {
    let mut context = empty_fixture_scope(producer_sha);
    // Admit the complete interface closure before selecting any native
    // owner. Type-only dependencies cannot be inferred as packages.
    let view = certified_product_artifact_view(
        producer_sha,
        &certified.recovery_products,
        &certified.module_interfaces,
        None,
    )
    .unwrap();
    if !exact_owners.is_empty() {
        context = context
            .extend_interface_artifacts(&view.interface_projection(exact_owners).unwrap())
            .unwrap();
    }
    if !lexical_roots.is_empty() {
        // Canonical interfaces retain authenticated source imports even
        // when a source-only owner has no executable recovery graph.
        // Native ownership and implementation roles remain separate.
        let imports = certified
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
            .collect();
        let surface = crate::declaration_join::source_lexical_closure(
            lexical_roots,
            &imports,
            &[],
            &view.source_implementation_roles(),
        )
        .unwrap();
        let lexical_owners = surface
            .lexical
            .iter()
            .map(|node| node.owner.clone())
            .collect::<Vec<_>>();
        context = context
            .extend_interface_artifacts(&view.interface_projection(&lexical_owners).unwrap())
            .unwrap();
        context = context.extend(&[], &[], surface.lexical).unwrap();
    }
    if !native_owners.is_empty() {
        context = context
            .extend_interface_artifacts(&view.interface_projection(native_owners).unwrap())
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
            let canonical = product
                .module_interface()
                .expect("native product must retain its paired finalized module");
            assert!(
                canonical.core_bytes().is_some(),
                "native scope requires its captured canonical interface and Core"
            );
            crate::certified_products::validate_original_module_interface(product, canonical)
                .expect("native custody must match its certified canonical module");
        }
        context = context
            .extend_checked_original_products(producer_sha, &native)
            .unwrap();
    }
    context
}

fn empty_fixture_scope(producer_sha: [u8; 32]) -> ExactDeclarationContext {
    ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_checked_original_products(producer_sha, &[])
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
        .records
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
    let selected =
        select_records_inner(producer, include, delivery, records, None).unwrap_or_else(|| {
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
    assert_eq!(fields.len(), 10);
    assert_eq!(text_field(&fields[0]), "TPSOURCEBOOTFIXTURE4");
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
    let context = if !matches!(fields[1], Value::Null) {
        let source_path = PathBuf::from(text_field(&fields[1]));
        assert!(source_path.is_absolute());
        let capture = PathBuf::from(text_field(&fields[9]));
        assert!(capture.is_absolute());
        // Source identity stays at its original authored path. The capture
        // supplies only the original bytes and payloads emitted at that path.
        let source = fs::read_to_string(capture.join("original-source.hs")).unwrap();
        let evidence_bytes = fs::read(capture.join("dependencies.json")).unwrap();
        let evidence = crate::cache::CompletedSourceEvidence::from_worker(
            &evidence_bytes,
            &source_path,
            &source,
        )
        .expect("actual consumed source and resolution evidence");
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
        let certified = certify_products(
            None,
            &receipt,
            &parsed,
            &evidence_bytes,
            &source_path,
            &capture,
            &evidence,
            &source,
            producer,
            &include,
            None,
            None,
        )
        .unwrap();
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
            &certified,
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
                &certified,
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
    let certificate = crate::declaration_join::certify_authored_declaration(
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
    let projected = context
        .artifact_view()
        .interface_projection(&[owner.clone()])
        .unwrap();
    let type_context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_interface_artifacts(&projected)
        .unwrap();
    assert!(type_context.recovery_products().is_empty());
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
