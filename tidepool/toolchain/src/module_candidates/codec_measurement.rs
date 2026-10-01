//! Opt-in measurements of the body codec using a retained production record.
//! The legacy representation belongs only to this fixture, never cache lookup.

use super::*;
use std::time::Instant;

#[derive(Clone, Serialize, Deserialize)]
struct LegacyRecord {
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
    package_imports: Vec<u8>,
    target_source: String,
}

impl LegacyRecord {
    fn into_current(self) -> Record {
        let mut record = Record {
            tag: self.tag,
            version: RECORD_VERSION,
            endpoint: self.endpoint,
            include: self.include,
            evidence: self.evidence,
            products: self.products,
            unit: self.unit,
            module: self.module,
            source: self.source,
            source_sha256: self.source_sha256,
            interface: self.interface,
            package_imports: self.package_imports,
            target_source: self.target_source,
            version_origin: CandidateVersionOrigin::Ordinary,
            original_owner: OriginalOwner {
                unit: String::new(),
                module: String::new(),
                module_version: [0; 32],
                skinny_iface_sha256: [0; 32],
                product_sha256: [0; 32],
            },
            original_certification: Vec::new(),
            execution_source_sha256: None,
            execution_source: None,
        };
        record.original_owner = OriginalOwner::from_owner(&computed_owner(&record));
        record
    }
}

fn buffers(record: &Record) -> serde_json::Value {
    serde_json::json!({
        "endpoint": {"bytes": record.endpoint.len(), "sha256": sha(&record.endpoint)},
        "products": {"bytes": record.products.len(), "sha256": sha(&record.products)},
        "interface": {"bytes": record.interface.len(), "sha256": sha(&record.interface)},
        "package_imports": {"bytes": record.package_imports.len(), "sha256": sha(&record.package_imports)},
        "module_version_hex": version_hash(record).iter().map(|b| format!("{b:02x}")).collect::<String>(),
    })
}

#[test]
#[ignore = "requires retained record path, explicit codec mode, and output path"]
fn retained_record_codec_measurement() {
    let path = PathBuf::from(std::env::var_os("TIDEPOOL_CODEC_INPUT").expect("retained input"));
    let output = PathBuf::from(std::env::var_os("TIDEPOOL_CODEC_OUTPUT").expect("output"));
    let mode = std::env::var("TIDEPOOL_CODEC_MODE").expect("before or after");
    assert!(matches!(mode.as_str(), "before" | "after"));
    assert!(fs::metadata(&path).unwrap().len() <= (RECORD_LIMIT + HEADER_LIMIT + 12) as u64);
    let file = fs::read(&path).unwrap();
    assert!(matches!(&file[..8], b"TPCREC6\n" | b"TPCREC7\n"));
    let header_len = u32::from_be_bytes(file[8..12].try_into().unwrap()) as usize;
    assert!(header_len <= HEADER_LIMIT && file.len() >= 12 + header_len);
    let header: RecordHeader = serde_json::from_slice(&file[12..12 + header_len]).unwrap();
    let payload = &file[12 + header_len..];
    assert!(payload.len() <= RECORD_LIMIT);
    assert_eq!(header.payload_len, payload.len() as u64);
    assert_eq!(header.payload_sha256, sha(payload));
    let legacy: LegacyRecord = ciborium::de::from_reader(payload).unwrap();
    let mut legacy_encoded = Vec::new();
    ciborium::ser::into_writer(&legacy, &mut legacy_encoded).unwrap();
    assert_eq!(
        legacy_encoded, payload,
        "fixture must reproduce the actual retained codec bytes"
    );
    let record = legacy.clone().into_current();
    let identity = buffers(&record);
    drop(legacy_encoded);

    // Fixture loading and identity hashing precede the timed codec windows.
    // Round zero is the first timed invocation, with filesystem pages already read.
    let mut rows = Vec::new();
    for round in 0..11 {
        let started = Instant::now();
        let mut encoded = Vec::new();
        if mode == "before" {
            ciborium::ser::into_writer(&legacy, &mut encoded).unwrap();
            let encode_ns = started.elapsed().as_nanos();
            let decode_started = Instant::now();
            let decoded: LegacyRecord = ciborium::de::from_reader(encoded.as_slice()).unwrap();
            let decode_ns = decode_started.elapsed().as_nanos();
            assert_eq!(buffers(&decoded.into_current()), identity);
            rows.push(serde_json::json!({"round":round,"encode_ns":encode_ns,"decode_ns":decode_ns,"payload_bytes":encoded.len()}));
        } else {
            ciborium::ser::into_writer(&record, &mut encoded).unwrap();
            let encode_ns = started.elapsed().as_nanos();
            let decode_started = Instant::now();
            let decoded: Record = ciborium::de::from_reader(encoded.as_slice()).unwrap();
            let decode_ns = decode_started.elapsed().as_nanos();
            assert_eq!(buffers(&decoded), identity);
            rows.push(serde_json::json!({"round":round,"encode_ns":encode_ns,"decode_ns":decode_ns,"payload_bytes":encoded.len()}));
        }
        assert!(encoded.len() <= RECORD_LIMIT);
        std::hint::black_box(&encoded);
    }
    fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
        "mode":mode,"input":path,"input_sha256":sha(&file),
        "retained_payload_sha256":sha(payload),"retained_payload_bytes":payload.len(),
        "unit":record.unit,"module":record.module,"original_buffers":identity,
        "record_version_after":RECORD_VERSION,"profile":"cargo test dev (unoptimized + debuginfo)",
        "scope":"CBOR record body codec; excludes framing, hashing, filesystem and compiler execution",
        "first_timed_round":0,"warm_timed_rounds":10,"rounds":rows
    })).unwrap()).unwrap();
}

/// Retained production bytes exercise the same normalized framing used by
/// certification, candidate publication and deployment export.
#[test]
#[ignore = "requires retained Core candidate and output path"]
fn retained_core_publication_framing_identity() {
    let path =
        PathBuf::from(std::env::var_os("TIDEPOOL_PUBLICATION_INPUT").expect("retained Core input"));
    let output =
        PathBuf::from(std::env::var_os("TIDEPOOL_PUBLICATION_OUTPUT").expect("identity output"));
    let record = read_record_path(&path).expect("authenticated bounded current record");
    assert_eq!(record.module, "Tidepool.Effects.Core");
    let requirements = crate::prepared_artifact::production_requirements().unwrap();
    let original = tidepool_repr::execution_schema::parse_module_products(
        &record.products,
        &requirements,
        product_decode_limits(),
    )
    .unwrap();
    let golden = split_module_product_bytes(&record.products, &original).unwrap();
    assert_eq!(golden, [record.products.clone()]);
    let identity = buffers(&record);
    let mut rows = Vec::new();
    let value: Value = ciborium::de::from_reader(record.products.as_slice()).unwrap();
    let fields = value.as_array().unwrap();
    let core_row = &fields[2].as_array().unwrap()[0];
    let unrelated = Value::Array(vec![
        Value::Text("unrelated".into()),
        Value::Text("Unrelated".into()),
        Value::Bytes(vec![0x43]),
        Value::Array(vec![]),
    ]);
    let mut combined = Vec::new();
    ciborium::ser::into_writer(&("TPMOD", 1u64, [core_row, &unrelated]), &mut combined).unwrap();
    let mut widened = vec![0x98, 3, 0x78, 5];
    widened.extend_from_slice(b"TPMOD");
    widened.extend_from_slice(&[0x18, 1, 0x98, 1, 0x98, 4]);
    assert_eq!(record.unit, "main");
    widened.extend_from_slice(&[0x78, 4]);
    widened.extend_from_slice(b"main");
    for field in &core_row.as_array().unwrap()[1..] {
        ciborium::ser::into_writer(field, &mut widened).unwrap();
    }
    for (case, bytes) in [
        ("original", &record.products),
        ("nonminimal_definite_widths", &widened),
        ("unrelated_module", &combined),
    ] {
        let (products, frames) =
            tidepool_repr::execution_schema::parse_module_products_with_framing(
                bytes,
                &requirements,
                product_decode_limits(),
            )
            .unwrap();
        assert_eq!(products[0], original[0]);
        assert_eq!(frames[0], golden[0]);
        let version = module_version_for_product(
            &record.endpoint,
            &record.include,
            &record.source_sha256,
            &record.interface,
            &frames[0],
            &record.package_imports,
        );
        assert_eq!(version, ModuleVersion(version_hash(&record)));
        rows.push(
            serde_json::json!({"case":case,"aggregate_bytes":bytes.len(),
            "owners":products.len(),"core_singleton_bytes":frames[0].len(),
            "core_singleton_sha256":sha(&frames[0]),"module_version_hex":hex(&version.0)}),
        );
    }
    fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
        "input":path,"input_sha256":sha(&fs::read(&path).unwrap()),
        "original_buffers":identity,"cases":rows,
        "scope":"Byte identity and original ownership only; no compiler or runtime latency claim"
    })).unwrap()).unwrap();
}
