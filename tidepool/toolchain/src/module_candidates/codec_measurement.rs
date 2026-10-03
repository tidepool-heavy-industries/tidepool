//! Opt-in measurements of the body codec using a retained production record.
//! The legacy representation belongs only to this fixture, never cache lookup.

use super::*;
use std::time::Instant;

// Inline evidence exists only in historical codec measurements and malformed
// fixture inputs. Production disk readers accept RecordData with EvidenceRef.
impl Serialize for Record {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&self.data, &mut encoded).map_err(serde::ser::Error::custom)?;
        let mut value: Value =
            ciborium::de::from_reader(encoded.as_slice()).map_err(serde::ser::Error::custom)?;
        encoded.clear();
        ciborium::ser::into_writer(&self.evidence, &mut encoded)
            .map_err(serde::ser::Error::custom)?;
        let evidence =
            ciborium::de::from_reader(encoded.as_slice()).map_err(serde::ser::Error::custom)?;
        let Value::Map(fields) = &mut value else {
            return Err(serde::ser::Error::custom("candidate record map"));
        };
        let (_, proof) = fields
            .iter_mut()
            .find(|(key, _)| key == &Value::Text("evidence".into()))
            .ok_or_else(|| serde::ser::Error::custom("candidate evidence field"))?;
        *proof = evidence;
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        let Value::Map(fields) = &mut value else {
            return Err(serde::de::Error::custom("candidate record map"));
        };
        let (_, proof) = fields
            .iter_mut()
            .find(|(key, _)| key == &Value::Text("evidence".into()))
            .ok_or_else(|| serde::de::Error::custom("candidate evidence field"))?;
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&*proof, &mut encoded).map_err(serde::de::Error::custom)?;
        let evidence: shared_evidence::SharedEvidence =
            ciborium::de::from_reader(encoded.as_slice()).map_err(serde::de::Error::custom)?;
        let reference = evidence
            .reference()
            .ok_or_else(|| serde::de::Error::custom("bounded dependency evidence"))?;
        encoded.clear();
        ciborium::ser::into_writer(reference, &mut encoded).map_err(serde::de::Error::custom)?;
        *proof = ciborium::de::from_reader(encoded.as_slice()).map_err(serde::de::Error::custom)?;
        encoded.clear();
        ciborium::ser::into_writer(&value, &mut encoded).map_err(serde::de::Error::custom)?;
        let data =
            ciborium::de::from_reader(encoded.as_slice()).map_err(serde::de::Error::custom)?;
        Ok(Self {
            data,
            evidence,
            module_interface_proof: None,
            execution_source: None,
        })
    }
}

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
        let evidence: shared_evidence::SharedEvidence = self.evidence.into();
        let mut record = Record {
            evidence: evidence.clone(),
            module_interface_proof: None,
            execution_source: None,
            data: RecordData {
                evidence: evidence.reference().unwrap().clone(),
                tag: self.tag,
                version: RECORD_VERSION,
                endpoint: self.endpoint,
                include: self.include,
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
                module_interface: None,
                execution_source_sha256: None,
            },
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
        "scope":"legacy inline in-memory record codec; excludes v11 shared-proof disk framing, hashing, filesystem and compiler execution",
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

/// The old inline proof format is accepted only by this opt-in retained fixture.
#[test]
#[ignore = "requires a private retained v10 producer directory and empty private cache"]
fn retained_shared_evidence_inventory_measurement() {
    let input = PathBuf::from(
        std::env::var_os("TIDEPOOL_SHARED_EVIDENCE_INPUT").expect("retained producer directory"),
    );
    let report =
        PathBuf::from(std::env::var_os("TIDEPOOL_SHARED_EVIDENCE_REPORT").expect("report output"));
    let mut paths = Vec::new();
    for shard in fs::read_dir(&input).unwrap() {
        let shard = shard.unwrap().path();
        if shard.is_dir() {
            for entry in fs::read_dir(shard).unwrap() {
                let path = entry.unwrap().path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "cbor")
                {
                    paths.push(path);
                }
            }
        }
    }
    paths.sort();
    assert_eq!(
        paths.len(),
        70,
        "the retained production fixture has 70 owner records"
    );
    let requirements = crate::prepared_artifact::production_requirements().unwrap();
    let mut old_bytes = 0usize;
    let mut new_bytes = 0usize;
    let mut old_proof_bytes = 0usize;
    let mut unique_proofs = BTreeMap::new();
    let mut owners = Vec::new();
    let mut groups = Vec::new();
    let mut symbols = BTreeSet::new();
    let mut globals = BTreeSet::new();
    let mut binder_references = 0usize;
    let mut global_references = 0usize;
    let mut referenced_graphs = BTreeSet::new();
    let mut endpoint = None;
    let mut include = None;
    for path in &paths {
        let file = fs::read(path).unwrap();
        assert_eq!(&file[..8], b"TPCREC9\n");
        let header_len = u32::from_be_bytes(file[8..12].try_into().unwrap()) as usize;
        assert!(header_len <= HEADER_LIMIT);
        let header: RecordHeader = serde_json::from_slice(&file[12..12 + header_len]).unwrap();
        let body = &file[12 + header_len..];
        assert!(body.len() <= RECORD_LIMIT);
        assert_eq!(header.payload_len, body.len() as u64);
        assert_eq!(header.payload_sha256, sha(body));
        let mut cursor = std::io::Cursor::new(body);
        let mut record: Record = ciborium::de::from_reader(&mut cursor).unwrap();
        assert_eq!(cursor.position(), body.len() as u64);
        assert_eq!(record.version, 8);
        assert!(RecordHeader::for_record(&record, body) == header);
        let identity = buffers(&record);
        record.version = RECORD_VERSION;
        let (reference, proof) = shared_evidence::encode_evidence(&record.evidence).unwrap();
        if let Some(digest) = record.execution_source_sha256 {
            referenced_graphs.insert(digest);
        }
        old_proof_bytes += proof.len();
        unique_proofs.insert(sha(&proof), proof.len());
        let producer_dir = record_dir(&record.endpoint);
        assert!(
            !producer_dir.starts_with(&input),
            "measurement must not mutate the retained input"
        );
        shared_evidence::publish(&producer_dir, &record.evidence).unwrap();
        let root = selected_record_root(&record).expect("unchanged current source selection");
        let shard = root_shard(&producer_dir, &root);
        fs::create_dir_all(&shard).unwrap();
        let framed = encode_record_checked(&record).unwrap();
        let frame_header_len = u32::from_be_bytes(framed[8..12].try_into().unwrap()) as usize;
        new_bytes += framed.len() - 12 - frame_header_len;
        old_bytes += body.len();
        let output = shard.join(path.file_name().unwrap());
        fs::write(&output, framed).unwrap();
        let restored = read_record_path(&output).unwrap();
        assert_eq!(buffers(&restored), identity);
        assert_eq!(
            restored.original_owner.owner(),
            record.original_owner.owner()
        );
        assert_eq!(
            restored.original_certification,
            record.original_certification
        );
        assert_eq!(
            serde_json::to_vec(&restored.evidence).unwrap(),
            serde_json::to_vec(&record.evidence).unwrap()
        );
        assert!(record.evidence.valid(&record.target_source));
        for product in tidepool_repr::execution_schema::parse_module_products(
            &record.products,
            &requirements,
            product_decode_limits(),
        )
        .unwrap()
        {
            for group in &product.groups {
                let inventory = group_inventory(group);
                let fields = inventory.as_array().unwrap();
                for binder in fields[1].as_array().unwrap() {
                    let mut bytes = Vec::new();
                    ciborium::ser::into_writer(binder, &mut bytes).unwrap();
                    symbols.insert(bytes);
                    binder_references += 1;
                }
                for global in fields[2].as_array().unwrap() {
                    let mut bytes = Vec::new();
                    ciborium::ser::into_writer(global, &mut bytes).unwrap();
                    globals.insert(bytes);
                    let mut symbol = Vec::new();
                    ciborium::ser::into_writer(&global.as_array().unwrap()[0], &mut symbol)
                        .unwrap();
                    symbols.insert(symbol);
                    global_references += 1;
                }
                groups.push(inventory);
            }
        }
        if let Some(expected) = &endpoint {
            assert_eq!(expected, &record.endpoint);
        } else {
            endpoint = Some(record.endpoint.clone());
        }
        if let Some(expected) = &include {
            assert_eq!(expected, &record.include);
        } else {
            include = Some(record.include.clone());
        }
        owners.push(serde_json::json!({"module":record.module,"identity":identity,"proof_storage_sha256":serde_json::to_value(reference).unwrap()}));
    }
    let endpoint = endpoint.unwrap();
    let producer_dir = record_dir(&endpoint);
    let mut graph_bytes = 0usize;
    let mut graph_count = 0usize;
    for entry in fs::read_dir(&input).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file()
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("execution-")
        {
            graph_bytes += fs::metadata(&path).unwrap().len() as usize;
            graph_count += 1;
            fs::copy(&path, producer_dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    let include = include.unwrap();
    let selected =
        ordinary_records(&endpoint, &include, true).expect("production aggregate read fits");
    assert_eq!(selected.len(), 70);
    let mut group_wire = Vec::new();
    ciborium::ser::into_writer(&Value::Array(groups.clone()), &mut group_wire).unwrap();
    let referenced_graph_bytes: usize = referenced_graphs
        .iter()
        .map(|digest| {
            fs::metadata(graph_path(&producer_dir, digest))
                .unwrap()
                .len() as usize
        })
        .sum();
    let symbol_indices: BTreeMap<_, _> = symbols
        .iter()
        .enumerate()
        .map(|(index, bytes)| (bytes.clone(), index))
        .collect();
    let global_indices: BTreeMap<_, _> = globals
        .iter()
        .enumerate()
        .map(|(index, bytes)| (bytes.clone(), index))
        .collect();
    let encoded = |value: &Value| {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    };
    let symbol_table = Value::Array(
        symbols
            .iter()
            .map(|bytes| ciborium::de::from_reader(bytes.as_slice()).unwrap())
            .collect(),
    );
    let global_table = Value::Array(
        globals
            .iter()
            .map(|bytes| {
                let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
                let mut fields = value.as_array().unwrap().clone();
                fields[0] = Value::Integer((symbol_indices[&encoded(&fields[0])] as u64).into());
                Value::Array(fields)
            })
            .collect(),
    );
    let indexed_groups = Value::Array(
        groups
            .iter()
            .map(|group| {
                let fields = group.as_array().unwrap();
                Value::Array(vec![
                    fields[0].clone(),
                    Value::Array(
                        fields[1]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|binder| {
                                Value::Integer((symbol_indices[&encoded(binder)] as u64).into())
                            })
                            .collect(),
                    ),
                    Value::Array(
                        fields[2]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|global| {
                                Value::Integer((global_indices[&encoded(global)] as u64).into())
                            })
                            .collect(),
                    ),
                ])
            })
            .collect(),
    );
    let indexed_inventory_bytes = encoded(&Value::Array(vec![
        symbol_table,
        global_table,
        indexed_groups,
    ]))
    .len();
    let trace_path = report.with_extension("selection.log");
    let subscriber = RetainedMeasurementSubscriber(trace_path);
    let selection = tracing::subscriber::with_default(subscriber, || {
        select_records_inner(
            &endpoint,
            &include,
            &producer_dir.join("offer-scratch"),
            selected
                .into_iter()
                .map(|record| (record, CandidateOrigin::Ordinary))
                .collect(),
            Some(&ExactCandidateContext::new(
                BTreeSet::new(),
                BTreeSet::new(),
            )),
        )
    });
    let selection = selection.expect("bounded compact production inventory offer");
    let selection_owners = selection.by_owner.len();
    assert_eq!(
        selection_owners, 70,
        "all original production owners are offered"
    );
    let manifest_bytes = fs::read(&selection.manifest_path).unwrap();
    assert!(manifest_bytes.len() <= MANIFEST_LIMIT);
    let manifest: Value = ciborium::de::from_reader(manifest_bytes.as_slice()).unwrap();
    let fields = manifest.as_array().unwrap();
    assert_eq!(fields.len(), 7);
    assert_eq!(fields[1].as_text(), Some("10"));
    let symbol_rows = fields[2].as_array().unwrap().len();
    let global_rows = fields[3].as_array().unwrap().len();
    assert_eq!(symbol_rows, symbols.len());
    assert_eq!(global_rows, globals.len());
    assert_eq!(fields[4].as_array().unwrap().len(), selection_owners);
    let distinct_proof_bytes: usize = unique_proofs.values().sum();
    assert!(old_bytes > PAYLOAD_LIMIT);
    assert!(new_bytes + distinct_proof_bytes < PAYLOAD_LIMIT);
    fs::write(report, serde_json::to_vec_pretty(&serde_json::json!({
        "scope":"private retained v10 conversion and production v11 ordinary record reader; no worker compilation or wall-clock speedup claim",
        "old_record_count":paths.len(),"old_payload_bytes":old_bytes,"old_inline_proof_bytes":old_proof_bytes,
        "new_payload_bytes":new_bytes,"unique_proofs":unique_proofs.len(),"unique_proof_bytes":distinct_proof_bytes,
        "new_aggregate_read_bytes":new_bytes+distinct_proof_bytes,"payload_limit":PAYLOAD_LIMIT,"production_records_recovered":70,"production_selection_owners":selection_owners,
        "group_rows":groups.len(),"group_inventory_cbor_bytes":group_wire.len(),"unique_symbols":symbols.len(),"unique_full_globals":globals.len(),
        "binder_references":binder_references,"global_references":global_references,"unique_symbol_cbor_bytes":symbols.iter().map(Vec::len).sum::<usize>(),
        "unique_global_cbor_bytes":globals.iter().map(Vec::len).sum::<usize>(),"graph_files":graph_count,"graph_file_bytes":graph_bytes,"referenced_graphs":referenced_graphs.len(),"referenced_graph_bytes":referenced_graph_bytes,
        "indexed_inventory_model_bytes":indexed_inventory_bytes,
        "actual_manifest_version":8,"actual_manifest_bytes":manifest_bytes.len(),
        "actual_shared_symbol_rows":symbol_rows,"actual_shared_global_rows":global_rows,
        "manifest_limit":MANIFEST_LIMIT,"owners":owners
    })).unwrap()).unwrap();
}

struct RetainedMeasurementSubscriber(PathBuf);
impl tracing::Subscriber for RetainedMeasurementSubscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(BTreeMap<String, String>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(field.name().into(), format!("{value:?}"));
            }
        }
        let mut fields = Fields(BTreeMap::new());
        event.record(&mut fields);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .unwrap();
        serde_json::to_writer(&mut file, &fields.0).unwrap();
        use std::io::Write;
        file.write_all(b"\n").unwrap();
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
