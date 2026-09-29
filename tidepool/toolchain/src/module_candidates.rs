//! Bounded durable storage for worker-certified module products. Records are
//! candidates only; the resident compiler must recheck their GHC semantics.

use ciborium::value::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tidepool_repr::execution_schema::{
    CachedHomeOwner, DecodeLimits, ModuleVersion, ProjectedGroup, RawModuleProduct,
    ResultContract, RuntimeRep, Signature, SymbolIdentity,
};

use crate::cache::{DependencyEvidence, ProductAvailability};

const RECORD_LIMIT: usize = 32 << 20;
const MANIFEST_LIMIT: usize = 4 << 20;
const CANDIDATE_LIMIT: usize = 128;
const RECORD_DIR: &str = "module-candidates-v5";
pub(crate) const PRODUCT_MAX_BYTES: usize = 32 << 20;

pub(crate) fn product_decode_limits() -> DecodeLimits {
    DecodeLimits {
        max_bytes: PRODUCT_MAX_BYTES,
        ..DecodeLimits::default()
    }
}

/// Preserve each original TPMOD row in its own bounded sidecar. A module's
/// product identity must not change when an unrelated module is compiled in
/// the same worker request.
pub(crate) fn split_module_product_bytes(
    bytes: &[u8],
    products: &[RawModuleProduct],
) -> Option<Vec<Vec<u8>>> {
    if bytes.len() > PRODUCT_MAX_BYTES {
        return None;
    }
    let Value::Array(header) = ciborium::de::from_reader::<Value, _>(bytes).ok()? else {
        return None;
    };
    let [
        Value::Text(magic),
        Value::Integer(version),
        Value::Array(rows),
    ] = <[Value; 3]>::try_from(header).ok()?
    else {
        return None;
    };
    if magic != "TPMOD" || version != 1.into() || rows.len() != products.len() {
        return None;
    }
    rows.into_iter()
        .zip(products)
        .map(|(row, product)| {
            let Value::Array(fields) = &row else {
                return None;
            };
            if !matches!(fields.as_slice(),
                [Value::Text(unit), Value::Text(module), Value::Bytes(_), Value::Array(_)]
                if unit == &product.unit && module == &product.module)
            {
                return None;
            }
            let sidecar = Value::Array(vec![
                Value::Text("TPMOD".into()),
                Value::Integer(1.into()),
                Value::Array(vec![row]),
            ]);
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&sidecar, &mut encoded).ok()?;
            (encoded.len() <= RECORD_LIMIT).then_some(encoded)
        })
        .collect()
}

fn identity_value(identity: &SymbolIdentity) -> Value {
    Value::Array(vec![
        Value::Text(identity.unit.clone()),
        Value::Text(identity.module.clone()),
        Value::Text(identity.namespace.clone()),
        Value::Text(identity.occurrence.clone()),
        identity.record_parent.clone().map_or(Value::Null, Value::Text),
    ])
}

fn rep_value(rep: RuntimeRep) -> Value {
    let (tag, bits) = match rep {
        RuntimeRep::Void => ("void", 0),
        RuntimeRep::LiftedRef => ("lifted", 0),
        RuntimeRep::UnliftedRef => ("unlifted", 0),
        RuntimeRep::Address => ("address", 0),
        RuntimeRep::Int(bits) => ("int", bits),
        RuntimeRep::Word(bits) => ("word", bits),
        RuntimeRep::Float(bits) => ("float", bits),
    };
    Value::Array(vec![Value::Text(tag.into()), Value::Integer(bits.into())])
}

fn signature_value(signature: &Signature) -> Value {
    let (tag, results) = match &signature.results {
        ResultContract::Returns(results) => ("returns", results.as_slice()),
        ResultContract::NoSuccess => ("no_success", &[][..]),
        ResultContract::CallerResult => ("caller_result", &[][..]),
    };
    Value::Array(vec![
        Value::Array(signature.arguments.iter().copied().map(rep_value).collect()),
        Value::Array(vec![
            Value::Text(tag.into()),
            Value::Array(results.iter().copied().map(rep_value).collect()),
        ]),
    ])
}

fn group_inventory(group: &ProjectedGroup) -> Value {
    let signatures = group.definitions();
    Value::Array(vec![
        Value::Integer(group.original_ordinal().into()),
        Value::Array(group.binders().iter().map(identity_value).collect()),
        Value::Array(group.globals().iter().map(|global| {
            Value::Array(vec![
                identity_value(&global.identity),
                rep_value(global.rep),
                global.entry_signature
                    .and_then(|id| signatures.signatures().get(id.0 as usize))
                    .map_or(Value::Null, signature_value),
                Value::Bool(global.required_evaluated),
                global.required_generation
                    .map_or(Value::Null, |generation| Value::Integer(generation.into())),
            ])
        }).collect()),
    ])
}

#[derive(Clone, Debug)]
pub(crate) struct CandidateBundle {
    pub owner: CachedHomeOwner,
    pub product: RawModuleProduct,
    pub source: PathBuf,
    pub source_sha256: String,
    pub iface_path: PathBuf,
    pub iface_sha256: String,
    pub product_bytes: Vec<u8>,
    pub evidence: DependencyEvidence,
    pub target_source: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CandidateSet {
    pub manifest_path: PathBuf,
    pub by_owner: BTreeMap<(String, String), CandidateBundle>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    tag: String,
    version: u32,
    endpoint: Vec<u8>,
    include: Vec<PathBuf>,
    evidence: DependencyEvidence,
    products: Vec<u8>,
    unit: String,
    module: String,
    source: PathBuf,
    source_sha256: String,
    interface: Vec<u8>,
    target_source: String,
}

fn sha(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn absolute(path: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(path).ok()?;
    canonical.is_absolute().then_some(canonical)
}

pub(crate) fn context_paths(include: &[PathBuf]) -> Option<Vec<PathBuf>> {
    include.iter().map(|p| absolute(p)).collect()
}

fn record_dir(endpoint_identity: &[u8], include: &[PathBuf]) -> PathBuf {
    let mut material = Vec::new();
    material.extend_from_slice(endpoint_identity);
    for path in include {
        material.push(0);
        material.extend_from_slice(path.as_os_str().as_encoded_bytes());
    }
    crate::paths::compile_cache_dir()
        .join(RECORD_DIR)
        .join(sha(&material))
}

/// Persist each eligible ordinary source module in its own bounded record.
pub(crate) fn publish(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    evidence: &DependencyEvidence,
    products: &[RawModuleProduct],
    product_bytes: &[u8],
    target_source: &str,
) {
    let Some(include) = context_paths(include) else {
        return;
    };
    if endpoint_identity.is_empty()
        || endpoint_identity.len() > 4096
        || product_bytes.len() > PRODUCT_MAX_BYTES
        || !evidence.valid(target_source)
        || !evidence.selection_complete
    {
        return;
    }
    let Ok(requirements) = crate::prepared_artifact::production_requirements() else {
        return;
    };
    let Ok(parsed) = tidepool_repr::execution_schema::parse_module_products(
        product_bytes,
        &requirements,
        product_decode_limits(),
    ) else {
        return;
    };
    if parsed != products {
        return;
    }
    let Some(per_module_bytes) = split_module_product_bytes(product_bytes, products) else {
        return;
    };
    let dir = record_dir(endpoint_identity, &include);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (product, module_bytes) in products.iter().zip(per_module_bytes) {
        if product.unit.is_empty()
            || product.module.is_empty()
            || product.interface.is_empty()
            || !parsed.iter().any(|p| {
                p.unit == product.unit
                    && p.module == product.module
                    && p.interface == product.interface
            })
        {
            continue;
        }
        let matching: Vec<_> = evidence
            .modules
            .iter()
            .filter(|m| {
                m.unit == product.unit
                    && m.module == product.module
                    && !m.boot
                    && m.product == ProductAvailability::Ready
            })
            .collect();
        if matching.len() != 1 {
            continue;
        }
        let source = &matching[0].source;
        let (source, source_sha256) = if source == Path::new("@generated-source") {
            continue;
        } else {
            let Some(path) = absolute(source) else {
                continue;
            };
            let Ok(bytes) = fs::read(&path) else { continue };
            (path, sha(&bytes))
        };
        if !evidence
            .sources
            .iter()
            .any(|s| absolute(&s.path).as_ref() == Some(&source) && s.sha256 == source_sha256)
        {
            continue;
        }
        let record = Record {
            tag: "TPMCAN".into(),
            version: 5,
            endpoint: endpoint_identity.to_vec(),
            include: include.clone(),
            evidence: evidence.clone(),
            products: module_bytes,
            unit: product.unit.clone(),
            module: product.module.clone(),
            source,
            source_sha256,
            interface: product.interface.clone(),
            target_source: target_source.to_owned(),
        };
        let mut bytes = Vec::new();
        if ciborium::ser::into_writer(&record, &mut bytes).is_err() || bytes.len() > RECORD_LIMIT {
            continue;
        }
        let mut key_material = Vec::new();
        key_material.extend_from_slice(&record.endpoint);
        key_material.extend_from_slice(record.unit.as_bytes());
        key_material.push(0);
        key_material.extend_from_slice(record.module.as_bytes());
        key_material.push(0);
        key_material.extend_from_slice(record.source.as_os_str().as_encoded_bytes());
        for root in &record.include {
            key_material.push(0);
            key_material.extend_from_slice(root.as_os_str().as_encoded_bytes());
        }
        let key = sha(&key_material);
        let _ = tidepool_atomic_write::write_best_effort(&dir.join(format!("{key}.cbor")), &bytes);
    }
}

fn version_hash(record: &Record) -> [u8; 32] {
    module_version_for_product(
        &record.endpoint,
        &record.include,
        &record.source_sha256,
        &record.interface,
        &record.products,
    )
    .0
}

/// The include paths must already be canonicalized by `context_paths`.
pub(crate) fn module_version_for_product(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    source_sha256: &str,
    interface: &[u8],
    products: &[u8],
) -> ModuleVersion {
    let mut h = Sha256::new();
    h.update(b"tidepool-module-candidate-v5\0");
    h.update(endpoint_identity);
    for path in include {
        h.update(path.as_os_str().as_encoded_bytes());
        h.update([0]);
    }
    h.update(source_sha256);
    h.update(interface);
    h.update(products);
    ModuleVersion(h.finalize().into())
}

/// Read and validate a deterministic, bounded subset of stored records.
pub(crate) fn select(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
) -> Option<CandidateSet> {
    let include = context_paths(include)?;
    let dir = record_dir(endpoint_identity, &include);
    let mut paths: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "cbor"))
        .collect();
    paths.sort();
    paths.truncate(CANDIDATE_LIMIT);
    let requirements = crate::prepared_artifact::production_requirements().ok()?;
    let mut by_owner = BTreeMap::new();
    let mut manifest = Vec::new();
    fs::create_dir_all(scratch).ok()?;
    let scratch = absolute(scratch)?;
    for path in paths {
        let Ok(bytes) = fs::read(path) else { continue };
        if bytes.len() > RECORD_LIMIT {
            continue;
        }
        let Ok(record) = ciborium::de::from_reader::<Record, _>(bytes.as_slice()) else {
            continue;
        };
        if record.tag != "TPMCAN"
            || record.version != 5
            || record.endpoint != endpoint_identity
            || record.include != include
            || record.source.is_relative()
        {
            continue;
        }
        let Ok(source_bytes) = fs::read(&record.source) else {
            continue;
        };
        if sha(&source_bytes) != record.source_sha256
            || !record.evidence.selection_complete
            || !record.evidence.valid(&record.target_source)
        {
            continue;
        }
        let Ok(parsed) = tidepool_repr::execution_schema::parse_module_products(
            &record.products,
            &requirements,
            product_decode_limits(),
        ) else {
            continue;
        };
        let matching: Vec<_> = parsed
            .into_iter()
            .filter(|p| {
                p.unit == record.unit
                    && p.module == record.module
                    && p.interface == record.interface
            })
            .collect();
        if matching.len() != 1 || record.interface.is_empty() {
            continue;
        }
        let evidence_count = record
            .evidence
            .modules
            .iter()
            .filter(|m| {
                m.unit == record.unit
                    && m.module == record.module
                    && !m.boot
                    && m.product == ProductAvailability::Ready
                    && absolute(&m.source).as_ref() == Some(&record.source)
            })
            .count();
        if evidence_count != 1
            || !record.evidence.sources.iter().any(|s| {
                absolute(&s.path).as_ref() == Some(&record.source)
                    && s.sha256 == record.source_sha256
            })
        {
            continue;
        }
        let iface_sha = Sha256::digest(&record.interface);
        let product_sha: [u8; 32] = Sha256::digest(&record.products).into();
        let Ok(evidence_bytes) = serde_json::to_vec(&record.evidence) else {
            continue;
        };
        let evidence_sha = sha(&evidence_bytes);
        let Some(module_evidence) = record.evidence.modules.iter().find(|module| {
            module.unit == record.unit
                && module.module == record.module
                && !module.boot
                && absolute(&module.source).as_ref() == Some(&record.source)
        }) else {
            continue;
        };
        let imports = module_evidence
            .imports
            .iter()
            .map(|imported| {
                Value::Array(vec![
                    Value::Text(String::from(imported.qualifier.clone())),
                    Value::Text(imported.module.clone()),
                    Value::Bool(imported.boot),
                    Value::Text(
                        imported
                            .selected
                            .as_ref()
                            .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
                    ),
                ])
            })
            .collect();
        let owner = CachedHomeOwner {
            unit: record.unit.clone(),
            module: record.module.clone(),
            module_version: ModuleVersion(version_hash(&record)),
            skinny_iface_sha256: iface_sha.into(),
            product_sha256: product_sha,
        };
        let iface_path = scratch.join(format!(
            "candidate-{}.hi",
            sha(format!("{}:{}", record.unit, record.module).as_bytes())
        ));
        if tidepool_atomic_write::write_best_effort(&iface_path, &record.interface).is_err() {
            continue;
        }
        let bundle = CandidateBundle {
            owner: owner.clone(),
            product: matching.into_iter().next()?,
            source: record.source.clone(),
            source_sha256: record.source_sha256.clone(),
            iface_path: iface_path.clone(),
            iface_sha256: sha(&record.interface),
            product_bytes: record.products.clone(),
            evidence: record.evidence.clone(),
            target_source: record.target_source.clone(),
        };
        if by_owner
            .insert((owner.unit.clone(), owner.module.clone()), bundle)
            .is_some()
        {
            return None;
        }
        let selected_product = &by_owner[&(record.unit.clone(), record.module.clone())].product;
        manifest.push(Value::Array(vec![
            Value::Text(owner.unit),
            Value::Text(owner.module),
            Value::Text(record.source.to_string_lossy().into_owned()),
            Value::Text(record.source_sha256),
            Value::Text(iface_path.to_string_lossy().into_owned()),
            Value::Text(sha(&record.interface)),
            Value::Text(hex(&owner.module_version.0)),
            Value::Text(hex(&product_sha)),
            Value::Text(evidence_sha),
            Value::Array(imports),
            Value::Array(selected_product.groups.iter().map(group_inventory).collect()),
        ]));
    }
    let value = Value::Array(vec![
        Value::Text("TPMCAN".into()),
        Value::Text("4".into()),
        Value::Array(manifest),
    ]);
    let mut encoded = Vec::new();
    ciborium::ser::into_writer(&value, &mut encoded).ok()?;
    if encoded.len() > MANIFEST_LIMIT {
        return None;
    }
    let manifest_path = scratch.join("module-candidates.cbor");
    tidepool_atomic_write::write_best_effort(&manifest_path, &encoded).ok()?;
    Some(CandidateSet {
        manifest_path,
        by_owner,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{ModuleEvidence, SourceEvidence};

    fn digest(bytes: &[u8]) -> String {
        sha(bytes)
    }

    fn product_bytes(unit: &str, module: &str, iface: &[u8]) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text("TPMOD".into()),
            Value::Integer(1.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text(unit.into()),
                Value::Text(module.into()),
                Value::Bytes(iface.into()),
                Value::Array(vec![]),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn unrelated_module_does_not_change_product_owner() {
        let first = product_bytes("u", "Library", &[0x42]);
        let second = product_bytes("u", "Unrelated", &[0x43]);
        let Value::Array(one) = ciborium::de::from_reader::<Value, _>(first.as_slice()).unwrap()
        else {
            unreachable!()
        };
        let Value::Array(two) = ciborium::de::from_reader::<Value, _>(second.as_slice()).unwrap()
        else {
            unreachable!()
        };
        let Value::Array(mut first_rows) = one[2].clone() else {
            unreachable!()
        };
        let Value::Array(second_rows) = two[2].clone() else {
            unreachable!()
        };
        first_rows.extend(second_rows);
        let mut combined = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                Value::Text("TPMOD".into()),
                Value::Integer(1.into()),
                Value::Array(first_rows),
            ]),
            &mut combined,
        )
        .unwrap();
        let requirements = crate::prepared_artifact::production_requirements().unwrap();
        let parsed = tidepool_repr::execution_schema::parse_module_products(
            &combined,
            &requirements,
            product_decode_limits(),
        )
        .unwrap();
        let split = split_module_product_bytes(&combined, &parsed).unwrap();
        assert_eq!(split[0], first);
        assert_eq!(split[1], second);
        assert_eq!(
            module_version_for_product(b"compiler", &[], "source-sha", &[0x42], &split[0]),
            module_version_for_product(b"compiler", &[], "source-sha", &[0x42], &first),
        );
    }

    fn write_record(root: &Path, source: &Path, unit: &str, module: &str, products: Vec<u8>) {
        let source = fs::canonicalize(source).unwrap();
        let source_bytes = fs::read(&source).unwrap();
        let target_source = "target".to_owned();
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: "@generated-source".into(),
                    sha256: digest(target_source.as_bytes()),
                },
                SourceEvidence {
                    path: source.clone(),
                    sha256: digest(&source_bytes),
                },
            ],
            resolutions: vec![],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: unit.into(),
                module: module.into(),
                boot: false,
                source: source.clone(),
                imports: vec![],
                product: ProductAvailability::Ready,
            }],
        };
        let record = Record {
            tag: "TPMCAN".into(),
            version: 5,
            endpoint: b"endpoint".to_vec(),
            include: vec![],
            evidence,
            products,
            unit: unit.into(),
            module: module.into(),
            source: source.clone(),
            source_sha256: digest(&source_bytes),
            interface: vec![0x42],
            target_source,
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&record, &mut bytes).unwrap();
        let dir = root.join(RECORD_DIR).join(sha(b"endpoint"));
        fs::create_dir_all(&dir).unwrap();
        let name = format!(
            "{}.cbor",
            sha(format!("{unit}:{module}:{}", source.display()).as_bytes())
        );
        fs::write(dir.join(name), bytes).unwrap();
    }

    fn select_in(root: &Path, scratch: &Path) -> Option<CandidateSet> {
        // The cache path is process-global; serialize this module's tests.
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root);
        }
        select(b"endpoint", &[], scratch)
    }

    #[test]
    #[serial_test::serial]
    fn selection_rejects_changed_source_and_corrupt_product_pair() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            product_bytes("u", "Library", &[0x42]),
        );
        fs::write(&source, "module Library where\nchanged").unwrap();
        assert!(
            select_in(root.path(), scratch.path())
                .unwrap()
                .by_owner
                .is_empty()
        );

        fs::write(&source, "module Library where").unwrap();
        let dir = root.path().join(RECORD_DIR).join(sha(b"endpoint"));
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            fs::remove_file(path).unwrap();
        }
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            b"broken cbor".to_vec(),
        );
        assert!(
            select_in(root.path(), scratch.path())
                .unwrap()
                .by_owner
                .is_empty()
        );
    }

    #[test]
    #[serial_test::serial]
    fn publish_and_select_accept_canonical_equivalent_source_paths() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            product_bytes("u", "Library", &[0x42]),
        );
        let dir = root.path().join(RECORD_DIR).join(sha(b"endpoint"));
        let path = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let mut record: Record = ciborium::de::from_reader(fs::File::open(&path).unwrap()).unwrap();
        fs::remove_file(path).unwrap();
        let equivalent = root.path().join("nested/../Library.hs");
        record.evidence.sources[1].path = equivalent.clone();
        record.evidence.modules[0].source = equivalent;
        let requirements = crate::prepared_artifact::production_requirements().unwrap();
        let products = tidepool_repr::execution_schema::parse_module_products(
            &record.products,
            &requirements,
            product_decode_limits(),
        )
        .unwrap();
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root.path());
        }
        publish(
            b"endpoint",
            &[],
            &record.evidence,
            &products,
            &record.products,
            &record.target_source,
        );
        let selected = select_in(root.path(), scratch.path()).unwrap();
        assert!(
            selected
                .by_owner
                .contains_key(&("u".into(), "Library".into()))
        );
    }

    #[test]
    #[serial_test::serial]
    fn selection_rejects_duplicate_module_owners() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            product_bytes("u", "Library", &[0x42]),
        );
        let dir = root.path().join(RECORD_DIR).join(sha(b"endpoint"));
        let original = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        fs::copy(original, dir.join("duplicate.cbor")).unwrap();
        assert!(select_in(root.path(), scratch.path()).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn selection_refuses_manifest_over_four_mebibytes() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        for n in 0..12 {
            let module = format!("{}{}", n, "M".repeat(390_000));
            write_record(
                root.path(),
                &source,
                "u",
                &module,
                product_bytes("u", &module, &[0x42]),
            );
        }
        assert!(select_in(root.path(), scratch.path()).is_none());
    }
}
