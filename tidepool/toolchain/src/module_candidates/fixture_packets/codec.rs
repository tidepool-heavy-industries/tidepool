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
            canonical_requirements: &BTreeMap::new(),
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

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodecOperation {
    CandidateEmpty,
    CandidateStructuralGroup,
    CandidateCompact,
    CandidateExpandedWithinBound,
    ReceiptFacts,
    CertificateFacts,
    InputFacts,
    Purpose,
    RequestTypes,
    ExpressionPurpose,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PurposeCase {
    Cell,
    Item,
    Inspection,
}
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HelperRecipe {
    None,
    ActorReply,
}
#[derive(Deserialize)]
struct NativeBytes(#[serde(deserialize_with = "deserialize_bytes")] Vec<u8>);
fn deserialize_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    struct Bytes;
    impl<'de> serde::de::Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("original owner-encoded bytes")
        }
        fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
            Ok(bytes.to_vec())
        }
        fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Self::Value, E> {
            Ok(bytes)
        }
    }
    deserializer.deserialize_byte_buf(Bytes)
}
enum CodecRequest {
    Candidate(CandidateCase),
    ReceiptFacts(PathBuf),
    CertificateFacts(PathBuf),
    InputFacts(PathBuf, PathBuf),
    Purpose(PurposeCase, Vec<PathBuf>, Option<NativeBytes>),
    RequestTypes(NativeBytes, HelperRecipe, Option<NativeBytes>),
    ExpressionPurpose(Vec<PathBuf>, NativeBytes),
}
fn decode_arguments<T: serde::de::DeserializeOwned>(value: &Value, fields: usize) -> T {
    assert_eq!(
        value.as_array().expect("codec argument tuple").len(),
        fields,
        "codec argument arity"
    );
    value.deserialized().expect("typed codec arguments")
}
fn packet_request(root: &Path) -> CodecRequest {
    assert!(root.is_absolute(), "codec request root must be absolute");
    let bytes = bounded_bytes(&root.join("request.cbor"), MANIFEST_LIMIT);
    let mut cursor = std::io::Cursor::new(&bytes);
    let value: Value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
        .expect("codec request CBOR");
    assert_eq!(
        cursor.position(),
        bytes.len() as u64,
        "codec request trailing bytes"
    );
    assert_eq!(
        value.as_array().expect("codec request tuple").len(),
        3,
        "codec request fields"
    );
    let (version, operation, arguments): (String, CodecOperation, Value) =
        value.deserialized().expect("typed codec request");
    assert_eq!(version, "TPCODECFIXTURE1", "codec request version");
    match operation {
        CodecOperation::CandidateEmpty
        | CodecOperation::CandidateStructuralGroup
        | CodecOperation::CandidateCompact
        | CodecOperation::CandidateExpandedWithinBound => {
            let _: [String; 0] = decode_arguments(&arguments, 0);
            CodecRequest::Candidate(match operation {
                CodecOperation::CandidateEmpty => CandidateCase::Empty,
                CodecOperation::CandidateStructuralGroup => CandidateCase::StructuralGroup,
                CodecOperation::CandidateCompact => CandidateCase::Compact,
                _ => CandidateCase::ExpandedWithinBound,
            })
        }
        CodecOperation::ReceiptFacts => {
            let [path] = decode_arguments(&arguments, 1);
            CodecRequest::ReceiptFacts(path)
        }
        CodecOperation::CertificateFacts => {
            let [path] = decode_arguments(&arguments, 1);
            CodecRequest::CertificateFacts(path)
        }
        CodecOperation::InputFacts => {
            let (proof, evidence) = decode_arguments(&arguments, 2);
            CodecRequest::InputFacts(proof, evidence)
        }
        CodecOperation::Purpose => {
            let (case, includes, signature) = decode_arguments(&arguments, 3);
            CodecRequest::Purpose(case, includes, signature)
        }
        CodecOperation::RequestTypes => {
            let (signatures, recipe, inner) = decode_arguments(&arguments, 3);
            CodecRequest::RequestTypes(signatures, recipe, inner)
        }
        CodecOperation::ExpressionPurpose => {
            let (includes, plan) = decode_arguments(&arguments, 2);
            CodecRequest::ExpressionPurpose(includes, plan)
        }
    }
}

#[test]
#[ignore = "explicit bounded codec request from the source-boot suite"]
fn source_boot_codec_packet_producer() {
    let root = PathBuf::from(
        std::env::var_os("TIDEPOOL_CANDIDATE_FIXTURE_PACKET")
            .expect("codec producer private request directory"),
    );
    match packet_request(&root) {
        CodecRequest::Candidate(case) => {
            let mut bytes = vec![];
            ciborium::ser::into_writer(&candidate_fixture(case), &mut bytes).unwrap();
            assert!(bytes.len() <= MANIFEST_LIMIT, "candidate packet bound");
            tidepool_atomic_write::write_best_effort(&root.join("module-candidates.cbor"), &bytes)
                .unwrap();
        }
        CodecRequest::ReceiptFacts(path) => receipt_facts(&root, absolute_path(&path)),
        CodecRequest::CertificateFacts(path) => certificate_facts(&root, absolute_path(&path)),
        CodecRequest::InputFacts(proof, evidence) => {
            input_facts(&root, absolute_path(&proof), absolute_path(&evidence))
        }
        CodecRequest::Purpose(case, includes, signature) => {
            purpose(&root, case, &includes, signature)
        }
        CodecRequest::RequestTypes(signatures, recipe, inner) => {
            request_types(&root, signatures, recipe, inner)
        }
        CodecRequest::ExpressionPurpose(includes, plan) => {
            expression_purpose(&root, &includes, plan)
        }
    }
}
fn absolute_path(path: &Path) -> &Path {
    assert!(path.is_absolute(), "codec input path must be absolute");
    path
}

fn bounded_bytes(path: &Path, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    fs::File::open(absolute_path(path))
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
fn receipt_facts(root: &Path, path: &Path) {
    let receipt = crate::declaration_context::read_exact_compilation_receipt(path)
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
fn certificate_facts(root: &Path, path: &Path) {
    let (receipt, inventory, references, global_sha256) =
        crate::certified_products::fixture_decoded_receipt(
            &bounded_bytes(path, MANIFEST_LIMIT),
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
        body_sha256: String,
    },
    UnsupportedBoot {
        unit: String,
        module: String,
        body_sha256: String,
    },
    UnsupportedWired {
        unit: String,
        module: String,
        imported_unit: String,
        imported_module: String,
        body_sha256: String,
    },
}
fn input_facts(root: &Path, proof: &Path, evidence_path: &Path) {
    use crate::compile_input::{CompileInputError, ValidatedInputPackages};
    let evidence_bytes = bounded_bytes(evidence_path, MANIFEST_LIMIT);
    let evidence: DependencyEvidence =
        serde_json::from_slice(&evidence_bytes).expect("typed dependency evidence");
    let (result, body_sha256) =
        ValidatedInputPackages::fixture_read_observed(proof, &evidence_bytes, &evidence)
            .expect("production compiler input packet decoder");
    let body_sha256 = hex(&body_sha256);
    let facts = match result {
        Ok(proof) => {
            let (closure, direct) = proof.fixture_observations();
            InputFacts::Checked {
                body_sha256,
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
        Err(CompileInputError::UnsupportedBoot { unit, module }) => InputFacts::UnsupportedBoot {
            unit,
            module,
            body_sha256,
        },
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
            body_sha256,
        },
        Err(failure) => panic!("production compiler input decoder refused: {failure}"),
    };
    write_facts(root, "input_facts", &facts);
}

fn purpose(root: &Path, case: PurposeCase, includes: &[PathBuf], signature: Option<NativeBytes>) {
    use crate::artifacts::checked_search_authorization;
    use crate::checked_cell::{
        encode_item_authorization, CheckedCellSpecification, CheckedItemKind, ItemAuthorization,
    };
    let signature = signature.map(|bytes| {
        crate::checked_cell::fixture_checked_signature(&bytes.0).expect("owner signature decoder")
    });
    let body = match case {
        PurposeCase::Cell => {
            assert!(
                signature.is_none(),
                "cell checking cannot carry an input signature"
            );
            crate::artifacts::encode_cell_authorization(
                crate::checked_cell::CheckedCellManifestPurpose::Authored,
                &CheckedCellSpecification {
                    admission_digest: [0xaa; 32],
                    cell_source: "codec cell".into(),
                    template_source: "codec template".into(),
                    turn_templates: vec![],
                    injected_modules: vec![],
                    reserved_declaration_modules: vec![],
                },
                Value::Array(vec![]),
                &crate::declaration_context::SelectedTemplateImports::default(),
            )
            .unwrap()
        }
        PurposeCase::Item => {
            assert!(signature.is_none(), "item purpose has no input signature");
            encode_item_authorization(ItemAuthorization {
                admission: [0xaa; 32],
                receipt: [0xaa; 32],
                index: 0,
                source: "codec item",
                kind: CheckedItemKind::Bind,
                binders: &[],
                templates: &[],
                injected: &[],
                signatures: &[],
                expression: None,
                generation: 1,
                runtime_prefix: [0xaa; 32],
                imports: &[],
                observation: None,
                planned: Value::Null,
                settled: vec![],
                value_interfaces: Value::Array(vec![]),
                template_imports: crate::declaration_context::SelectedTemplateImports::default(),
            })
        }
        PurposeCase::Inspection => {
            assert!(signature.is_none(), "inspection has no input signature");
            crate::artifacts::fixture_inspection_authorization()
        }
    };
    let value = checked_search_authorization(body, includes).expect("purpose search encoder");
    let mut bytes = vec![];
    ciborium::ser::into_writer(&value, &mut bytes).unwrap();
    tidepool_atomic_write::write_best_effort(&root.join("purpose.cbor"), &bytes).unwrap();
}

fn request_types(
    root: &Path,
    bytes: NativeBytes,
    recipe: HelperRecipe,
    inner: Option<NativeBytes>,
) {
    use crate::declaration_context::{encode_request_authorization, RequestHelperRecipe};
    let signatures = crate::checked_cell::RequestTypeSignatures::from_bytes(&bytes.0)
        .expect("owner request signature decoder");
    let recipe = match recipe {
        HelperRecipe::None => RequestHelperRecipe::None,
        HelperRecipe::ActorReply => RequestHelperRecipe::ActorReply,
    };
    let purpose = inner.map(|bytes| {
        assert!(bytes.0.len() <= MANIFEST_LIMIT, "inner purpose bound");
        let mut cursor = std::io::Cursor::new(&bytes.0);
        let value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
            .expect("inner owner purpose");
        assert_eq!(
            cursor.position(),
            bytes.0.len() as u64,
            "inner purpose trailing bytes"
        );
        value
    });
    let mut bytes = vec![];
    ciborium::ser::into_writer(
        &encode_request_authorization(&signatures, recipe, purpose),
        &mut bytes,
    )
    .unwrap();
    tidepool_atomic_write::write_best_effort(&root.join("purpose.cbor"), &bytes).unwrap();
}

fn expression_purpose(root: &Path, includes: &[PathBuf], plan: NativeBytes) {
    use crate::artifacts::checked_search_authorization;
    use crate::checked_cell::{encode_item_authorization, CheckedItemKind, ItemAuthorization};
    let expression = crate::checked_cell::fixture_expression_plan(&plan.0)
        .expect("owner reserved expression plan reader");
    let body = encode_item_authorization(ItemAuthorization {
        admission: [0xaa; 32],
        receipt: [0xaa; 32],
        index: 0,
        source: "1",
        kind: CheckedItemKind::Expression,
        binders: &[],
        templates: &[],
        injected: &[],
        signatures: &[],
        expression: Some(&expression),
        generation: 1,
        runtime_prefix: [0xaa; 32],
        imports: &[],
        observation: Some("observation"),
        planned: Value::Null,
        settled: vec![],
        value_interfaces: Value::Array(vec![]),
        template_imports: crate::declaration_context::SelectedTemplateImports::default(),
    });
    let value = checked_search_authorization(body, includes).unwrap();
    let mut bytes = vec![];
    ciborium::ser::into_writer(&value, &mut bytes).unwrap();
    tidepool_atomic_write::write_best_effort(&root.join("purpose.cbor"), &bytes).unwrap();
}
