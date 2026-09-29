//! Bounded durable storage for worker-certified module products. Records are
//! candidates only; the resident compiler must recheck their GHC semantics.

use ciborium::value::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tidepool_repr::execution_schema::{
    CachedHomeOwner, DecodeLimits, ModuleVersion, RawModuleProduct,
};

use crate::cache::{DependencyEvidence, ProductAvailability};

const RECORD_LIMIT: usize = 32 << 20;
const MANIFEST_LIMIT: usize = 1 << 20;
const CANDIDATE_LIMIT: usize = 128;
const RECORD_DIR: &str = "module-candidates-v2";

#[derive(Clone, Debug)]
pub(crate) struct CandidateBundle {
    pub owner: CachedHomeOwner,
    pub product: RawModuleProduct,
    pub source: PathBuf,
    pub source_sha256: String,
    pub iface_path: PathBuf,
    pub iface_sha256: String,
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

fn context_paths(include: &[PathBuf]) -> Option<Vec<PathBuf>> {
    include.iter().map(|p| absolute(p)).collect()
}

fn record_dir() -> PathBuf {
    crate::paths::compile_cache_dir().join(RECORD_DIR)
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
        || product_bytes.len() > RECORD_LIMIT / 2
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
        DecodeLimits::default(),
    ) else {
        return;
    };
    if parsed != products {
        return;
    }
    let dir = record_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    for product in products {
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
            .any(|s| s.path == source && s.sha256 == source_sha256)
        {
            continue;
        }
        let record = Record {
            tag: "TPMCAN".into(),
            version: 2,
            endpoint: endpoint_identity.to_vec(),
            include: include.clone(),
            evidence: evidence.clone(),
            products: product_bytes.to_vec(),
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
    let mut h = Sha256::new();
    h.update(b"tidepool-module-candidate-v2\0");
    h.update(&record.endpoint);
    for path in &record.include {
        h.update(path.as_os_str().as_encoded_bytes());
        h.update([0]);
    }
    h.update(&record.source_sha256);
    h.update(&record.interface);
    h.update(&record.products);
    if let Ok(evidence) = serde_json::to_vec(&record.evidence) {
        h.update(evidence);
    }
    h.finalize().into()
}

/// Read and validate a deterministic, bounded subset of stored records.
pub(crate) fn select(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
) -> Option<CandidateSet> {
    let include = context_paths(include)?;
    let dir = record_dir();
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
            || record.version != 2
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
            DecodeLimits::default(),
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
                    && m.source == record.source
            })
            .count();
        if evidence_count != 1
            || !record
                .evidence
                .sources
                .iter()
                .any(|s| s.path == record.source && s.sha256 == record.source_sha256)
        {
            continue;
        }
        let iface_sha = Sha256::digest(&record.interface);
        let product_sha: [u8; 32] = Sha256::digest(&record.products).into();
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
        };
        if by_owner
            .insert((owner.unit.clone(), owner.module.clone()), bundle)
            .is_some()
        {
            return None;
        }
        manifest.push(Value::Array(vec![
            Value::Text(owner.unit),
            Value::Text(owner.module),
            Value::Text(record.source.to_string_lossy().into_owned()),
            Value::Text(record.source_sha256),
            Value::Text(iface_path.to_string_lossy().into_owned()),
            Value::Text(sha(&record.interface)),
            Value::Text(hex(&owner.module_version.0)),
            Value::Text(hex(&product_sha)),
        ]));
    }
    let value = Value::Array(vec![
        Value::Text("TPMCAN".into()),
        Value::Text("2".into()),
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
            version: 2,
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
        let dir = root.join(RECORD_DIR);
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
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());

        fs::write(&source, "module Library where").unwrap();
        let dir = root.path().join(RECORD_DIR);
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
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());
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
        let dir = root.path().join(RECORD_DIR);
        let original = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        fs::copy(original, dir.join("duplicate.cbor")).unwrap();
        assert!(select_in(root.path(), scratch.path()).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn selection_refuses_manifest_over_one_mebibyte() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        for n in 0..3 {
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
