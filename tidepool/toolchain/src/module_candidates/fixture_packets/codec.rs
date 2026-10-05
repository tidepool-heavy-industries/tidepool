//! Bounded structural codec controls. These packets carry no execution parcel.
use super::super::inventory::{GlobalKey, InventoryTables, StructuralGroup};
use super::super::*;

#[derive(Clone, Copy)]
enum CandidateCase {
    Empty,
    StructuralGroup,
    Compact,
    ExpandedWithinBound,
}

fn fixture_identity() -> SymbolIdentity {
    SymbolIdentity {
        unit: "main".into(),
        module: "Fixture".into(),
        namespace: "value".into(),
        occurrence: "entry".into(),
        record_parent: None,
    }
}

fn candidate_fixture(case: CandidateCase) -> Value {
    let identity = fixture_identity();
    let (groups, reverse) = match case {
        CandidateCase::Empty => (Vec::new(), false),
        CandidateCase::StructuralGroup => (
            vec![StructuralGroup {
                ordinal: 3,
                binders: vec![identity],
                globals: vec![],
            }],
            false,
        ),
        CandidateCase::ExpandedWithinBound => {
            let mut identity = identity;
            identity.occurrence = "x".repeat(2048);
            (
                vec![StructuralGroup {
                    ordinal: 0,
                    binders: vec![identity; 1536],
                    globals: vec![],
                }],
                false,
            )
        }
        CandidateCase::Compact => {
            let mut identities = vec![identity.clone(); 6];
            identities[1].record_parent = Some("Parent".into());
            identities[2].unit = "other".into();
            identities[3].module = "Other".into();
            identities[4].namespace = "data".into();
            identities[5].occurrence = "other".into();
            let plain = GlobalKey {
                identity: identity.clone(),
                rep: RuntimeRep::LiftedRef,
                signature: None,
                evaluated: false,
                generation: None,
            };
            let mut globals = vec![plain; 10];
            globals[1].rep = RuntimeRep::Int(64);
            for (index, arguments, results) in [
                (
                    2,
                    vec![RuntimeRep::LiftedRef],
                    ResultContract::Returns(vec![RuntimeRep::Int(64)]),
                ),
                (
                    3,
                    vec![RuntimeRep::Address],
                    ResultContract::Returns(vec![RuntimeRep::Int(64)]),
                ),
                (
                    4,
                    vec![RuntimeRep::LiftedRef],
                    ResultContract::Returns(vec![RuntimeRep::Word(64)]),
                ),
                (5, vec![RuntimeRep::LiftedRef], ResultContract::NoSuccess),
                (6, vec![RuntimeRep::LiftedRef], ResultContract::CallerResult),
            ] {
                globals[index].signature = Some(Signature { arguments, results });
            }
            globals[7].evaluated = true;
            globals[8].generation = Some(0);
            globals[9].generation = Some(7);
            (
                vec![
                    StructuralGroup {
                        ordinal: 91,
                        binders: identities,
                        globals: globals.clone(),
                    },
                    StructuralGroup {
                        ordinal: 3,
                        binders: vec![identity],
                        globals: globals.into_iter().rev().collect(),
                    },
                ],
                true,
            )
        }
    };
    let mut inventory = InventoryTables::structural(&groups).expect("bounded structural inventory");
    let encode_row = |name: &str, groups| {
        candidate_manifest_row(CandidateManifestRow {
            unit: "main",
            module: name,
            source: Path::new("/fixture/source.hs"),
            source_sha256: &"0".repeat(64),
            interface: Path::new("/fixture/interface.hi"),
            interface_sha256: &"0".repeat(64),
            module_version: &"0".repeat(64),
            product_sha256: &"0".repeat(64),
            evidence_sha256: &"0".repeat(64),
            imports: vec![],
            groups,
            packages: Path::new("/fixture/packages"),
            packages_sha256: &"0".repeat(64),
            product: Path::new("/fixture/products.tpmod"),
            canonical_requirements: vec![],
            certificate: Path::new("/fixture/module.cbor"),
            certificate_sha256: &[0xaa; 32],
            core: Path::new("/fixture/Core"),
            core_sha256: &[0xaa; 32],
        })
    };
    let mut rows = vec![encode_row(
        "Fixture",
        inventory.structural_groups(&groups).unwrap(),
    )];
    if reverse {
        let reversed = groups.into_iter().rev().collect::<Vec<_>>();
        rows.push(encode_row(
            "Other",
            inventory.structural_groups(&reversed).unwrap(),
        ));
    }
    let (symbols, globals) = inventory.into_wire_tables();
    candidate_manifest_value(symbols, globals, rows, vec![], vec![], &"a".repeat(64))
}

fn packet_request(root: &Path) -> (String, Vec<Value>) {
    assert!(root.is_absolute(), "codec request root must be absolute");
    let path = root.join("request.cbor");
    let bytes = bounded_bytes(&path, MANIFEST_LIMIT);
    assert!(bytes.len() <= MANIFEST_LIMIT, "codec request bound");
    let mut cursor = std::io::Cursor::new(&bytes);
    let request: Value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
        .expect("codec request CBOR");
    assert_eq!(
        cursor.position(),
        bytes.len() as u64,
        "codec request trailing bytes"
    );
    let request = request.as_array().expect("codec request tuple");
    assert_eq!(request.len(), 3, "codec request fields");
    assert_eq!(
        request[0].as_text(),
        Some("TPCODECFIXTURE1"),
        "codec request version"
    );
    (
        request[1].as_text().expect("codec operation").into(),
        request[2].as_array().expect("codec arguments").clone(),
    )
}

#[test]
#[ignore = "explicit bounded codec request from the source-boot suite"]
fn source_boot_codec_packet_producer() {
    let Some(root) = std::env::var_os("TIDEPOOL_CANDIDATE_FIXTURE_PACKET") else {
        panic!("codec producer requires its private request directory");
    };
    let root = PathBuf::from(root);
    let (operation, arguments) = packet_request(&root);
    match operation.as_str() {
        "candidate_empty"
        | "candidate_structural_group"
        | "candidate_compact"
        | "candidate_expanded_within_bound" => {
            assert!(
                arguments.is_empty(),
                "candidate scenario takes no arguments"
            );
            let case = match operation.as_str() {
                "candidate_empty" => CandidateCase::Empty,
                "candidate_structural_group" => CandidateCase::StructuralGroup,
                "candidate_compact" => CandidateCase::Compact,
                _ => CandidateCase::ExpandedWithinBound,
            };
            let mut bytes = vec![];
            ciborium::ser::into_writer(&candidate_fixture(case), &mut bytes).unwrap();
            assert!(bytes.len() <= MANIFEST_LIMIT, "candidate packet bound");
            tidepool_atomic_write::write_best_effort(&root.join("module-candidates.cbor"), &bytes)
                .unwrap();
        }
        "receipt_facts" => receipt_facts(&root, &arguments),
        "certificate_facts" => certificate_facts(&root, &arguments),
        "input_facts" => input_facts(&root, &arguments),
        "canonical_module" => canonical_module(&root, &arguments),
        "purpose" => purpose(&root, &arguments),
        "request_types" => request_types(&root, &arguments),
        _ => panic!("unsupported codec operation: {operation}"),
    }
}

fn argument_path(value: &Value) -> PathBuf {
    let path = PathBuf::from(value.as_text().expect("codec path argument"));
    assert!(path.is_absolute(), "codec input path must be absolute");
    path
}
fn bounded_bytes(path: &Path, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .expect("codec input")
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .expect("codec input read");
    assert!(bytes.len() <= limit, "codec input bound");
    bytes
}
fn write_facts(root: &Path, operation: &str, facts: &impl Serialize) {
    let mut bytes = vec![];
    ciborium::ser::into_writer(&("TPCODECFACTS1", operation, facts), &mut bytes)
        .expect("typed observation CBOR");
    assert!(
        bytes.len() <= 16 * MANIFEST_LIMIT,
        "codec observation bound"
    );
    tidepool_atomic_write::write_best_effort(&root.join("facts.cbor"), &bytes).unwrap();
}

#[derive(Serialize)]
struct OwnerFact {
    unit: String,
    module: String,
}
impl OwnerFact {
    fn from_pair(owner: &(String, String)) -> Self {
        Self {
            unit: owner.0.clone(),
            module: owner.1.clone(),
        }
    }
}
#[derive(Serialize)]
struct ReceiptFacts {
    source_path: PathBuf,
    cache_safe: bool,
    source_selected: Vec<OwnerFact>,
}
fn receipt_facts(root: &Path, arguments: &[Value]) {
    assert_eq!(arguments.len(), 1, "receipt facts arguments");
    let receipt =
        crate::declaration_context::read_exact_compilation_receipt(&argument_path(&arguments[0]))
            .expect("production exact receipt decoder");
    write_facts(
        root,
        "receipt_facts",
        &ReceiptFacts {
            source_path: receipt.source_path,
            cache_safe: receipt.evidence.cache_safe,
            source_selected: receipt
                .claims
                .into_iter()
                .map(|claim| OwnerFact {
                    unit: claim.owner.unit,
                    module: claim.owner.module,
                })
                .collect(),
        },
    );
}

#[derive(Serialize)]
struct IdentityFact {
    unit: String,
    module: String,
    namespace: String,
    occurrence: String,
    record_parent: Option<String>,
}
impl From<&SymbolIdentity> for IdentityFact {
    fn from(identity: &SymbolIdentity) -> Self {
        Self {
            unit: identity.unit.clone(),
            module: identity.module.clone(),
            namespace: identity.namespace.clone(),
            occurrence: identity.occurrence.clone(),
            record_parent: identity.record_parent.clone(),
        }
    }
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ImportOwnerFact {
    Source {
        unit: String,
        module: String,
        version: Option<String>,
        ordinal: u32,
        binder: IdentityFact,
    },
    Retained {
        binder: IdentityFact,
        generation: u64,
    },
    Package {
        unit: String,
        module: String,
        interface_sha256: String,
        binder: IdentityFact,
    },
    RetainedPackage {
        unit: String,
        module: String,
        interface_sha256: String,
        binder: IdentityFact,
        generation: u64,
    },
}
impl From<&crate::certified_products::ReceiptImportOwner> for ImportOwnerFact {
    fn from(owner: &crate::certified_products::ReceiptImportOwner) -> Self {
        use crate::certified_products::ReceiptImportOwner;
        match owner {
            ReceiptImportOwner::Source {
                unit,
                module,
                module_version,
                original_ordinal,
                binder,
            } => Self::Source {
                unit: unit.clone(),
                module: module.clone(),
                version: module_version.as_ref().map(|v| hex(&v.0)),
                ordinal: *original_ordinal,
                binder: binder.into(),
            },
            ReceiptImportOwner::Retained {
                identity,
                generation,
            } => Self::Retained {
                binder: identity.into(),
                generation: *generation,
            },
            ReceiptImportOwner::Package {
                unit,
                module,
                binder,
                interface_digest,
            } => Self::Package {
                unit: unit.clone(),
                module: module.clone(),
                interface_sha256: hex(interface_digest),
                binder: binder.into(),
            },
            ReceiptImportOwner::RetainedPackage {
                unit,
                module,
                binder,
                interface_digest,
                generation,
            } => Self::RetainedPackage {
                unit: unit.clone(),
                module: module.clone(),
                interface_sha256: hex(interface_digest),
                binder: binder.into(),
                generation: *generation,
            },
        }
    }
}
#[derive(Serialize)]
struct ModuleFact {
    unit: String,
    module: String,
    ordinals: Vec<u32>,
}
#[derive(Serialize)]
struct PackageFact {
    unit: String,
    module: String,
    interface_path: PathBuf,
    interface_sha256: String,
}
#[derive(Serialize)]
struct TargetFact {
    name: String,
    references: Vec<usize>,
}
#[derive(Serialize)]
struct CertificateFacts {
    owners: Vec<ImportOwnerFact>,
    global_sha256: Vec<String>,
    packages: Vec<PackageFact>,
    modules: Vec<ModuleFact>,
    targets: Vec<TargetFact>,
}
fn certificate_facts(root: &Path, arguments: &[Value]) {
    assert_eq!(arguments.len(), 1, "certificate facts arguments");
    let path = argument_path(&arguments[0]);
    let (receipt, inventory, references, global_sha256) =
        crate::certified_products::fixture_decoded_receipt(
            &bounded_bytes(&path, MANIFEST_LIMIT),
            path.parent(),
        )
        .expect("production product receipt decoder");
    write_facts(
        root,
        "certificate_facts",
        &CertificateFacts {
            global_sha256: global_sha256.iter().map(|digest| hex(digest)).collect(),
            owners: inventory
                .iter()
                .map(|global| (&global.owner).into())
                .collect(),
            packages: receipt
                .packages
                .iter()
                .map(|((unit, module), witness)| PackageFact {
                    unit: unit.clone(),
                    module: module.clone(),
                    interface_path: witness.selected_path.clone(),
                    interface_sha256: hex(&witness.sha256),
                })
                .collect(),
            targets: references
                .into_iter()
                .map(|(name, references)| TargetFact { name, references })
                .collect(),
            modules: receipt
                .modules
                .into_iter()
                .map(|module| ModuleFact {
                    unit: module.unit,
                    module: module.module,
                    ordinals: module
                        .groups
                        .iter()
                        .map(|group| group.original_ordinal)
                        .collect(),
                })
                .collect(),
        },
    );
}

#[derive(Serialize)]
struct DirectInputFact {
    owner: OwnerFact,
    packages: Vec<OwnerFact>,
}
#[derive(Serialize)]
#[serde(tag = "category", rename_all = "kebab-case")]
enum InputFacts {
    Checked {
        direct: Vec<DirectInputFact>,
        closure: Vec<OwnerFact>,
    },
    UnsupportedBoot {
        unit: String,
        module: String,
    },
    UnsupportedWired {
        unit: String,
        module: String,
        imported_unit: String,
        imported_module: String,
    },
}
fn input_facts(root: &Path, arguments: &[Value]) {
    use crate::compile_input::{CompileInputError, ValidatedInputPackages};
    assert_eq!(arguments.len(), 2, "input facts arguments");
    let evidence_bytes = bounded_bytes(&argument_path(&arguments[1]), MANIFEST_LIMIT);
    let evidence: DependencyEvidence =
        serde_json::from_slice(&evidence_bytes).expect("typed dependency evidence");
    let facts = match ValidatedInputPackages::read(
        &argument_path(&arguments[0]),
        &evidence_bytes,
        &evidence,
    ) {
        Ok(proof) => {
            let (closure, direct) = proof.fixture_observations();
            InputFacts::Checked {
                closure: closure.iter().map(OwnerFact::from_pair).collect(),
                direct: direct
                    .iter()
                    .map(|(owner, packages)| DirectInputFact {
                        owner: OwnerFact::from_pair(owner),
                        packages: packages.iter().map(OwnerFact::from_pair).collect(),
                    })
                    .collect(),
            }
        }
        Err(CompileInputError::UnsupportedBoot { unit, module }) => {
            InputFacts::UnsupportedBoot { unit, module }
        }
        Err(CompileInputError::UnsupportedWiredInput {
            unit,
            module,
            imported_unit,
            imported_module,
        }) => InputFacts::UnsupportedWired {
            unit,
            module,
            imported_unit,
            imported_module,
        },
        Err(failure) => panic!("production compiler input decoder refused: {failure}"),
    };
    write_facts(root, "input_facts", &facts);
}

fn canonical_module(root: &Path, arguments: &[Value]) {
    use crate::certified_products::{
        CapturedArtifactDescriptor, FinalizationEnvelope, FinalizedModuleReceipt,
    };
    assert_eq!(arguments.len(), 7, "canonical codec arguments");
    let unit = arguments[0].as_text().expect("canonical unit");
    let module = arguments[1].as_text().expect("canonical module");
    assert!(!unit.is_empty() && !module.is_empty(), "canonical owner");
    let source = argument_path(&arguments[2]);
    let interface = argument_path(&arguments[3]);
    let packages = bounded_bytes(&argument_path(&arguments[4]), MANIFEST_LIMIT);
    let core = bounded_bytes(&argument_path(&arguments[5]), 8 * MANIFEST_LIMIT);
    let home_units = arguments[6]
        .as_array()
        .expect("canonical home units")
        .iter()
        .map(|value| value.as_text().expect("home unit").to_owned())
        .collect::<BTreeSet<_>>();
    assert!(
        !home_units.is_empty()
            && home_units.len() <= 128
            && home_units.iter().all(|u| !u.is_empty()),
        "home unit bound"
    );
    let source_sha = Sha256::digest(bounded_bytes(&source, 32 * MANIFEST_LIMIT)).into();
    let interface_bytes = bounded_bytes(&interface, 16 * MANIFEST_LIMIT);
    let interface_sha = Sha256::digest(&interface_bytes).into();
    let descriptor = |path: &str, bytes: &[u8]| CapturedArtifactDescriptor {
        relative_path: path.into(),
        sha256: Sha256::digest(bytes).into(),
        bytes: bytes.len() as u64,
    };
    let receipt = FinalizedModuleReceipt {
        unit: unit.into(),
        module: module.into(),
        source_sha256: source_sha,
        interface: CapturedArtifactDescriptor {
            relative_path: "module.hi".into(),
            sha256: interface_sha,
            bytes: interface_bytes.len() as u64,
        },
        package_imports: descriptor("captured.packages", &packages),
        core: Some(descriptor("captured.core", &core)),
        interface_requirements: BTreeMap::new(),
    };
    let certificate = crate::certified_products::fixture_canonical_certificate(
        [0xaa; 32],
        &FinalizationEnvelope {
            profile: crate::certified_products::FINALIZATION_PROFILE.into(),
            home_units,
            modules: BTreeMap::new(),
        },
        &receipt,
    )
    .expect("canonical owner encoder");
    let core_path = root.join("captured.core");
    let package_path = root.join("captured.packages");
    let certificate_path = root.join("captured.certificate");
    let product = root.join("descriptor.tpmod");
    for (path, bytes) in [
        (&core_path, core.as_slice()),
        (&package_path, packages.as_slice()),
        (&certificate_path, certificate.as_slice()),
        (&product, [].as_slice()),
    ] {
        tidepool_atomic_write::write_best_effort(path, bytes).unwrap();
    }
    let row = candidate_manifest_row(CandidateManifestRow {
        unit,
        module,
        source: &source,
        source_sha256: &hex(&source_sha),
        interface: &interface,
        interface_sha256: &hex(&interface_sha),
        module_version: &"0".repeat(64),
        product_sha256: &sha(&[]),
        evidence_sha256: &"0".repeat(64),
        imports: vec![],
        groups: Value::Array(vec![]),
        packages: &package_path,
        packages_sha256: &sha(&packages),
        product: &product,
        canonical_requirements: vec![],
        certificate: &certificate_path,
        certificate_sha256: &Sha256::digest(&certificate).into(),
        core: &core_path,
        core_sha256: &Sha256::digest(&core).into(),
    });
    let mut bytes = vec![];
    ciborium::ser::into_writer(
        &candidate_manifest_value(
            Value::Array(vec![]),
            Value::Array(vec![]),
            vec![row],
            vec![],
            vec![],
            &"a".repeat(64),
        ),
        &mut bytes,
    )
    .unwrap();
    tidepool_atomic_write::write_best_effort(&root.join("module-candidates.cbor"), &bytes).unwrap();
}

fn purpose(root: &Path, arguments: &[Value]) {
    use crate::artifacts::{checked_search_authorization, CheckedPurpose};
    use crate::checked_cell::{
        encode_display_authorization, encode_item_authorization, CheckedCellSpecification,
        CheckedExpressionPresentation, CheckedItemKind, DisplayAuthorization, ItemAuthorization,
    };
    assert_eq!(arguments.len(), 3, "purpose codec arguments");
    let case = arguments[0].as_text().expect("purpose scenario");
    let includes = arguments[1]
        .as_array()
        .expect("purpose includes")
        .iter()
        .map(argument_path)
        .collect::<Vec<_>>();
    let signature = match &arguments[2] {
        Value::Null => None,
        Value::Bytes(bytes) => Some(
            crate::checked_cell::fixture_checked_signature(bytes).expect("owner signature decoder"),
        ),
        _ => panic!("purpose signature must be original compiler bytes or null"),
    };
    let (stage, body) = match case {
        "cell" | "host-input-check" => {
            let host = case == "host-input-check";
            assert_eq!(
                signature.is_some(),
                host,
                "only host input check carries a native signature"
            );
            (
                if host {
                    CheckedPurpose::HostInputCell
                } else {
                    CheckedPurpose::Cell
                },
                crate::artifacts::encode_cell_authorization(
                    &CheckedCellSpecification {
                        admission_digest: [0xaa; 32],
                        cell_source: if host {
                            "sessionInput <- pure (undefined :: TidepoolActivationInput)"
                        } else {
                            "codec cell"
                        }
                        .into(),
                        template_source: "codec template".into(),
                        turn_templates: if host {
                            vec![("bind".into(), "codec input template".into())]
                        } else {
                            vec![]
                        },
                        injected_modules: vec![],
                        reserved_declaration_modules: vec![],
                    },
                    Value::Array(vec![]),
                    signature.as_ref(),
                )
                .unwrap(),
            )
        }
        "item" | "host-activation-input" => {
            let host = case == "host-activation-input";
            assert_eq!(
                signature.is_some(),
                host,
                "only host input carries a native signature"
            );
            let binders = if host {
                vec!["sessionInput".into()]
            } else {
                vec![]
            };
            let templates = if host {
                vec![("bind".into(), "codec host input template".into())]
            } else {
                vec![]
            };
            let signatures = signature.into_iter().collect::<Vec<_>>();
            let body = encode_item_authorization(ItemAuthorization {
                admission: [0xaa; 32],
                receipt: [0xaa; 32],
                index: 0,
                source: "sessionInput <- pure 1",
                kind: CheckedItemKind::Bind,
                binders: &binders,
                templates: &templates,
                injected: &[],
                signatures: &signatures,
                expression: None,
                generation: 1,
                runtime_prefix: [0xaa; 32],
                imports: &[],
                observation: None,
                planned: Value::Null,
                settled: vec![],
                value_interfaces: Value::Array(vec![]),
                template_interfaces: Value::Array(vec![]),
            });
            (
                if host {
                    CheckedPurpose::HostActivationInput
                } else {
                    CheckedPurpose::Item
                },
                body,
            )
        }
        "display" => {
            assert!(signature.is_none(), "display has no input signature");
            (
                CheckedPurpose::Display,
                encode_display_authorization(DisplayAuthorization {
                    item_admission: [0xaa; 32],
                    receipt: [0xaa; 32],
                    index: 0,
                    observation: "observation",
                    captured_generation: 1,
                    generation: 2,
                    admission: [0xaa; 32],
                    budget: 32,
                    presented: &[],
                    templates: &[],
                    injected: &[],
                    imports: &[],
                    presentation: CheckedExpressionPresentation::Rendered,
                    planned: Value::Null,
                    settled: vec![],
                    value_interfaces: Value::Array(vec![]),
                    template_interfaces: Value::Array(vec![]),
                }),
            )
        }
        "inspection" => {
            assert!(signature.is_none(), "inspection has no input signature");
            (
                CheckedPurpose::Inspection,
                crate::artifacts::fixture_inspection_authorization(),
            )
        }
        _ => panic!("unsupported purpose codec scenario"),
    };
    let value =
        checked_search_authorization(stage, body, &includes).expect("purpose search encoder");
    let mut bytes = vec![];
    ciborium::ser::into_writer(&value, &mut bytes).unwrap();
    tidepool_atomic_write::write_best_effort(&root.join("purpose.cbor"), &bytes).unwrap();
}

fn request_types(root: &Path, arguments: &[Value]) {
    use crate::declaration_context::{encode_request_authorization, RequestHelperRecipe};
    assert_eq!(arguments.len(), 3, "request type codec arguments");
    let bytes = arguments[0]
        .as_bytes()
        .expect("original request signature bytes");
    let signatures = crate::checked_cell::RequestTypeSignatures::from_bytes(bytes)
        .expect("owner request signature decoder");
    let recipe = match arguments[1].as_text().expect("request helper recipe") {
        "none" => RequestHelperRecipe::None,
        "actor-reply" => RequestHelperRecipe::ActorReply,
        _ => panic!("unknown request helper recipe"),
    };
    let purpose = match &arguments[2] {
        Value::Null => None,
        Value::Bytes(bytes) => {
            assert!(bytes.len() <= MANIFEST_LIMIT, "inner purpose bound");
            let mut cursor = std::io::Cursor::new(bytes);
            let value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
                .expect("inner owner purpose");
            assert_eq!(
                cursor.position(),
                bytes.len() as u64,
                "inner purpose trailing bytes"
            );
            Some(value)
        }
        _ => panic!("inner owner purpose bytes or null required"),
    };
    let mut bytes = vec![];
    ciborium::ser::into_writer(
        &encode_request_authorization(&signatures, recipe, purpose),
        &mut bytes,
    )
    .unwrap();
    tidepool_atomic_write::write_best_effort(&root.join("purpose.cbor"), &bytes).unwrap();
}
