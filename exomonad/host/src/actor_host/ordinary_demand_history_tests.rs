//! Ordinary notebook work through the shipped prepared actor and its tool dispatcher.
//! Exporter breadth and source-free host reopen are separate histories.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    actor_output_origin, commit_workspace, hosted_script_provider, hosted_test_settings,
    next_hosted_script_round, prepare_performance_traces, record_phase,
};
use harness::model::{AgentPath, OperationId};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufRead, Seek};
use std::path::Path;
use tidepool_repr::execution_schema::{CachedHomeOwner, InventoryDecodeLimits};

const PROBE: &str = "Project.OrdinaryDemandProbe";

#[derive(Debug, serde::Serialize)]
struct ExpectedOriginal {
    unit: String,
    module: String,
    module_version: String,
    interface: String,
    product: String,
    available: BTreeSet<u32>,
    entry: u32,
    later: u32,
}

fn digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl ExpectedOriginal {
    fn from_product(owner: &CachedHomeOwner, bytes: &[u8]) -> Self {
        let products = tidepool_repr::execution_schema::parse_module_products(
            bytes,
            &tidepool_toolchain::prepared_artifact::production_requirements().unwrap(),
            InventoryDecodeLimits::default(),
        )
        .unwrap();
        let [product] = products.as_slice() else {
            panic!("one authenticated original per retained product")
        };
        assert_eq!(owner.unit, "main");
        assert_eq!(owner.module, PROBE);
        assert_eq!(
            (&product.unit, &product.module),
            (&owner.unit, &owner.module)
        );
        let ordinal = |occurrence: &str| {
            let matching = product
                .groups
                .iter()
                .filter(|group| {
                    group.binders().iter().any(|binder| {
                        binder.unit == owner.unit
                            && binder.module == owner.module
                            && binder.namespace == "value"
                            && binder.record_parent.is_none()
                            && binder.occurrence == occurrence
                    })
                })
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "one original for {occurrence}");
            matching[0].original_ordinal()
        };
        let result = Self {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: digest(&owner.module_version.0),
            interface: digest(&owner.skinny_iface_sha256),
            product: digest(&owner.product_sha256),
            available: product
                .groups
                .iter()
                .map(|group| group.original_ordinal())
                .collect(),
            entry: ordinal("ordinaryEntry"),
            later: ordinal("ordinaryLater"),
        };
        assert_ne!(result.entry, result.later);
        result
    }

    fn matches(&self, fields: &Value, ordinal: u32) -> bool {
        fields["group_unit"] == self.unit
            && fields["group_module"] == self.module
            && fields["group_module_version"] == self.module_version
            && fields["group_iface_sha256"] == self.interface
            && fields["group_product_sha256"] == self.product
            && fields["original_ordinal"].as_u64() == Some(ordinal as u64)
    }
}

fn trace_offset(path: &Path) -> u64 {
    std::fs::metadata(path)
        .expect("the existing trace owner created its file")
        .len()
}

// Read only complete owner-emitted records; no subscriber or compiler route is
// installed here. Each cell has a bounded trace slice retained by the runner.
fn trace_rows(path: &Path, offset: u64) -> Vec<Value> {
    let file = std::fs::File::open(path).unwrap();
    let mut reader = std::io::BufReader::new(file);
    reader.seek(std::io::SeekFrom::Start(offset)).unwrap();
    let mut result = Vec::new();
    let mut bytes = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        assert!(
            line.ends_with('\n'),
            "trace owner must finish its record before settlement"
        );
        bytes += line.len();
        assert!(
            bytes <= 64 * 1024 * 1024,
            "bounded diagnostic bytes per ordinary cell"
        );
        assert!(
            result.len() < 100_000,
            "bounded diagnostic rows per ordinary cell"
        );
        result.push(serde_json::from_str(&line).expect("complete trace JSON"));
    }
    result
}

fn execution_matches(row: &Value, execution: &str) -> bool {
    row["spans"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(std::iter::once(&row["span"]))
        .any(|span| span["name"] == "cell" && span["execution"] == execution)
}

type ImageKey = (u64, String, u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
enum GroupWorkStage {
    #[serde(rename = "raw_projection")]
    Projected,
    #[serde(rename = "raw_projection_acquired")]
    Acquired,
    #[serde(rename = "raw_projection_reuse")]
    Reused,
    #[serde(rename = "raw_encoding_demand")]
    EncodingDemand,
    #[serde(rename = "accepted_output")]
    Accepted,
}

#[derive(serde::Deserialize)]
struct GroupWorkRow {
    worker_pid: u64,
    projection_observation: Option<u64>,
    stage: GroupWorkStage,
    unit: String,
    module: String,
    ordinals: BTreeSet<u32>,
    refused_ordinals: BTreeSet<u32>,
    complete: bool,
}

fn validate_group_work(rows: &[serde_json::Value]) {
    let typed = rows
        .iter()
        .map(|row| serde_json::from_value::<GroupWorkRow>(row.clone()).unwrap())
        .collect::<Vec<_>>();
    let mut observed: std::collections::BTreeMap<(u64, u64), &GroupWorkRow> =
        std::collections::BTreeMap::new();
    let mut owners = std::collections::BTreeMap::new();
    for row in &typed {
        assert!(row.complete);
        assert!(row.ordinals.is_disjoint(&row.refused_ordinals));
        let Some(token) = row.projection_observation else {
            continue;
        };
        let key = (row.worker_pid, token);
        if let Some(previous) = owners.insert(key, (&row.unit, &row.module)) {
            assert_eq!(
                (&row.unit, &row.module),
                previous,
                "one token retains its exact raw owner"
            );
        }
        if row.stage != GroupWorkStage::Accepted {
            if let Some(previous) = observed.insert(key, row) {
                assert_eq!(
                    (&row.ordinals, &row.refused_ordinals),
                    (&previous.ordinals, &previous.refused_ordinals),
                    "same raw result retains group outcomes across acquisition and encoding demand"
                );
            }
        }
    }
    for row in &typed {
        let Some(token) = row.projection_observation else {
            continue;
        };
        if row.stage == GroupWorkStage::Accepted {
            if let Some(raw) = observed.get(&(row.worker_pid, token)) {
                assert_eq!((&row.unit, &row.module), (&raw.unit, &raw.module));
                assert!(
                    row.ordinals.is_subset(&raw.ordinals),
                    "accepted output is selected from the same observed raw result"
                );
            }
            // A prior-request token with no local raw row remains unknown.
        }
    }
}

fn flattened(row: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    for source in row["spans"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(std::iter::once(&row["span"]))
        .chain(std::iter::once(&row["fields"]))
    {
        if let Some(object) = source.as_object() {
            fields.extend(object.clone());
        }
    }
    Value::Object(fields)
}

fn same_request(fields: &Value, request: &Value) -> bool {
    [
        "daemon_epoch",
        "admission_id",
        "request_ordinal",
        "compile_request",
    ]
    .iter()
    .all(|key| !fields[key].is_null() && fields[key] == request[key])
}

fn physical_key(fields: &Value) -> (String, u64, u64) {
    let epoch = fields["daemon_epoch"]
        .as_str()
        .expect("attribution UNKNOWN: daemon epoch");
    let admission = fields["admission_id"]
        .as_u64()
        .expect("attribution UNKNOWN: admission identity");
    let ordinal = fields["request_ordinal"]
        .as_u64()
        .expect("attribution UNKNOWN: physical request ordinal");
    let logical = fields["compile_request"]
        .as_str()
        .expect("attribution UNKNOWN: logical request correlation");
    assert!(
        admission > 0 && ordinal > 0,
        "attribution UNKNOWN: invalid physical identity"
    );
    for (value, length) in [(epoch, 64), (logical, 16)] {
        assert!(
            value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "attribution UNKNOWN: malformed request identity"
        );
    }
    (epoch.to_owned(), admission, ordinal)
}

fn daemon_work(rows: &[Value], requests: &[Value], unrelated: &[Value]) -> Vec<Value> {
    let fields = rows.iter().map(flattened).collect::<Vec<_>>();
    let identified = requests
        .iter()
        .map(|request| (request, true))
        .chain(
            unrelated
                .iter()
                .map(|row| (&row["physical_request"], false)),
        )
        .collect::<Vec<_>>();
    let mut physical = BTreeMap::new();
    for (request, _) in &identified {
        assert!(
            physical.insert(physical_key(request), *request).is_none(),
            "attribution UNKNOWN: duplicate host request identification"
        );
    }
    // The independent daemon census must not disappear merely because the
    // corresponding host identification was lost from its retained slice.
    for row in fields.iter().filter(|row| {
        // Request-scoped timing/work is independently observable even if all
        // lifecycle records were lost. Partial physical identities also refuse.
        ["daemon_epoch", "admission_id", "request_ordinal"]
            .iter()
            .any(|key| row.get(key).is_some())
            || matches!(
                row["message"].as_str(),
                Some(
                    "compiler request started"
                        | "compiler request finished"
                        | "compiler diagnostics processed"
                )
            )
    }) {
        let request = physical
            .get(&physical_key(row))
            .expect("attribution UNKNOWN: daemon request lacks host identification");
        assert!(
            same_request(row, request),
            "attribution UNKNOWN: conflicting physical request correlation"
        );
    }
    identified
        .iter()
        .map(|(request, related)| {
            let observed = fields
                .iter()
                .filter(|row| same_request(row, request))
                .collect::<Vec<_>>();
            let started = observed
                .iter()
                .filter(|row| row["message"] == "compiler request started")
                .collect::<Vec<_>>();
            let finished = observed
                .iter()
                .filter(|row| row["message"] == "compiler request finished")
                .collect::<Vec<_>>();
            let processed = observed
                .iter()
                .filter(|row| row["message"] == "compiler diagnostics processed")
                .count();
            assert_eq!(
                (started.len(), finished.len(), processed),
                (1, 1, 1),
                "attribution UNKNOWN: exact completed physical request and consumed worker diagnostics"
            );
            let exit_code = finished[0]["exit_code"].as_i64()
                .expect("attribution UNKNOWN: physical terminal outcome");
            if *related {
                assert_eq!(exit_code, 0);
            }
            let mode = started[0]["request_mode"]
                .as_str()
                .expect("typed physical request mode");
            assert!(
                matches!(
                    mode,
                    "cell_plan"
                        | "cell_program"
                        | "declaration_interface"
                        | "check_source"
                        | "classify"
                        | "inspection"
                        | "turn"
                        | "source"
                        | "activation_preview"
                ),
                "unknown request mode {mode}"
            );
            let work = observed
                .iter()
                .filter_map(|row| row["line"].as_str())
                .filter_map(|line| line.strip_prefix("tidepool-group-work "))
                .map(|payload| serde_json::from_str::<Value>(payload).unwrap())
                .collect::<Vec<_>>();
            validate_group_work(&work);
            let worker_timing = observed
                .iter()
                .filter_map(|row| row["line"].as_str())
                .filter(|line| line.starts_with("tidepool-timing "))
                .count();
            // Parsing/interface-only requests do not promise executable groups.
            // An absent group row is reported as such, never converted to zero
            // physical evaluation or a measured unused-body saving.
            json!({"physical_request":request, "mode":mode,
            "attribution":if *related {"operation"} else {"explicit_unrelated"},
            "terminal":finished[0], "worker_timing_rows":worker_timing,
            "group_observation_status":if work.is_empty() {"not_emitted"} else {"observed"},
            "work":work, "encoding_counts_are_demands_not_evaluations":true})
        })
        .collect()
}

fn attribute_requests(rows: &[Value], execution: &str) -> (Vec<Value>, Vec<Value>) {
    // Census every identified submission in this retained call slice before
    // attributing it. A lost cell span is unknown, never a cache-hit zero.
    let mut requests = Vec::new();
    let mut unrelated = Vec::new();
    let mut all_physical = BTreeSet::new();
    for row in rows
        .iter()
        .filter(|row| row["fields"]["message"] == "compiler request identified")
    {
        let fields = &row["fields"];
        assert!(
            all_physical.insert(physical_key(fields)),
            "one identification per physical request"
        );
        assert_eq!(
            fields["transport"], "daemon",
            "unclassified compiler transport"
        );
        if execution_matches(row, execution) {
            requests.push(fields.clone());
        } else {
            let issuer = rows
                .iter()
                .find(|dispatch| {
                    let issued = &dispatch["fields"];
                    issued["message"] == "workbench cell dispatched to its actor"
                        && issued["execution"]
                            .as_str()
                            .is_some_and(|other| execution_matches(row, other))
                        && issued["turn_id"].as_str().is_some()
                        && issued["context_call_id"]
                            .as_str()
                            .is_some_and(|call| !call.is_empty())
                })
                .expect("unbound compiler observation: attribution UNKNOWN, not zero work");
            unrelated.push(json!({"physical_request":fields, "issuer":issuer["fields"]}));
        }
    }
    (requests, unrelated)
}

fn observe_cell(
    rows: &[Value],
    operation: &OperationId,
    expected: &ExpectedOriginal,
    ordinal: u32,
) -> (Vec<Value>, BTreeMap<ImageKey, u64>, Vec<Value>) {
    let dispatch = rows
        .iter()
        .filter(|row| {
            let fields = &row["fields"];
            fields["message"] == "workbench cell dispatched to its actor"
                && fields["turn_id"] == operation.request.0
                && fields["context_call_id"] == operation.call.0
        })
        .collect::<Vec<_>>();
    assert_eq!(
        dispatch.len(),
        1,
        "one actual dispatch for the issued operation"
    );
    let execution = dispatch[0]["fields"]["execution"].as_str().unwrap();
    let observed = rows
        .iter()
        .filter(|row| execution_matches(row, execution))
        .collect::<Vec<_>>();
    let (requests, unrelated) = attribute_requests(rows, execution);
    let uses = observed
        .iter()
        .filter(|row| row["fields"]["message"] == "certified source import resolved")
        .map(|row| &row["fields"])
        .collect::<Vec<_>>();
    assert!(
        uses.iter().any(|fields| expected.matches(fields, ordinal)),
        "the actual validated target walk consumes its expected published original"
    );
    let mut actual = BTreeMap::<u64, u64>::new();
    for fields in &uses {
        *actual
            .entry(fields["target_image_instance"].as_u64().unwrap())
            .or_default() += 1;
    }
    let mut completed = BTreeMap::<u64, u64>::new();
    for row in &observed {
        let fields = &row["fields"];
        if fields["message"] == "certified source import resolution complete" {
            *completed
                .entry(fields["target_image_instance"].as_u64().unwrap())
                .or_default() += fields["source_imports_resolved"].as_u64().unwrap();
        }
    }
    for (target, count) in actual {
        assert_eq!(
            completed.get(&target),
            Some(&count),
            "successful complete install-resolution census"
        );
    }
    // Exact inherited source use may bypass ImageRegistry entirely. A registry
    // decision is optional; only an actual ready decision has an image identity.
    let mut images = BTreeMap::new();
    for row in &observed {
        let fields = &row["fields"];
        if fields["message"] == "native group image association"
            && expected.matches(fields, ordinal)
        {
            assert_eq!(fields["outcome"], "ready");
            let key = (
                fields["image_registry"].as_u64().unwrap(),
                fields["key_domain"].as_str().unwrap().to_owned(),
                fields["image_entry"].as_u64().unwrap(),
            );
            images.insert(key, fields["image_instance"].as_u64().unwrap());
        }
    }
    (requests, images, unrelated)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the matched prepared workspace, owned compiler and complete host/daemon traces"]
async fn prepared_actor_entry_late_binding_and_warm_repeat_observe_actual_work() {
    assert_eq!(super::test_campaign::campaign_trace_profile(), "full");
    let (host_trace, compiler_trace, phases, _, capture) = prepare_performance_traces();
    assert_eq!(
        capture["status"], "installed",
        "an existing unverified subscriber is not completeness evidence"
    );
    assert_eq!(capture["profile"], "full");
    assert!(
        trace_rows(&compiler_trace, 0).iter().any(|row| {
            row["fields"]["message"] == "compiler trace profile applied"
                && row["fields"]["profile"] == "full"
        }),
        "the actual owned daemon applied the requested complete profile"
    );
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, |config| {
        super::scaffold_admission_tests::prepared_scaffold(config);
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("Project/OrdinaryDemandProbe.hs"),
            include_str!("fixtures/ordinary_demand_probe.hs"),
        )
        .unwrap();
        crate::exomonad::edit_fixture_project_config(&authored, |project| {
            project.haskell.modules.push(PROBE.into());
        });
        // The installed spec's actual lexical closure, rather than the wider
        // preparation inventory, is the source-publication boundary.
        let spec_path = authored.join("AgentSpec.hs");
        let spec = std::fs::read_to_string(&spec_path).unwrap();
        let declaration = "module AgentSpec (agentSpec) where";
        assert_eq!(
            spec.matches(declaration).count(),
            1,
            "the shipped spec template"
        );
        std::fs::write(
            &spec_path,
            spec.replacen(
                declaration,
                &format!("{declaration}\n\nimport {PROBE} ()"),
                1,
            ),
        )
        .unwrap();
        commit_workspace(&config.workspace);
    })
    .await
    .expect("the shipped preparation and InitialTools owner publishes its originals");
    host.run_scenario(|host| Box::pin(async move {
        let original = host.assert_prepared_workspace_original_handoff().await;
        let products = original.products().recovery_products.iter()
            .filter(|product| product.owner().unit == "main" && product.owner().module == PROBE)
            .collect::<Vec<_>>();
        assert_eq!(products.len(), 1, "one exact Probe original in the installed prepared deployment");
        let expected = ExpectedOriginal::from_product(products[0].owner(), products[0].product_bytes());
        record_phase(&phases, json!({"phase":"installed-original", "expected":expected,
            "host_trace":host_trace, "compiler_trace":compiler_trace,
            "encoding_counts_are_demands_not_evaluations":true}));
        host.http_input("Run the small original-demand notebook history.").await.unwrap();
        let root = AgentPath("/root".into());
        let origin = actor_output_origin(host.runtime.run_identity(), host.context.actor.identity());
        let mut pending = VecDeque::new();
        let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        let mut prior_images = BTreeMap::new();
        for (label, binding, value, ordinal) in [
            ("entry", "ordinaryEntry", "42", expected.entry),
            ("later", "ordinaryLater", "73", expected.later),
            ("warm-repeat", "ordinaryLater", "73", expected.later),
        ] {
            let call = format!("ordinary-demand-{label}");
            let operation = round.operation(&call);
            let offset = trace_offset(&host_trace);
            let compiler_offset = trace_offset(&compiler_trace);
            let source = format!("import qualified {PROBE} as Probe\ndisplay (Probe.{binding} :: Int)");
            let started = std::time::Instant::now();
            round.call(&call, &source);
            round = host.while_operation_succeeds(&operation,
                next_hosted_script_round(&mut requests, &mut pending, &root))
                .await.expect("the exact ordinary cell succeeds and produces its successor request");
            round.assert_authored_value(&host.runtime.store(), &origin, &call, value);
            let operation_elapsed_ns = started.elapsed().as_nanos();
            let observation_started = std::time::Instant::now();
            let rows = trace_rows(&host_trace, offset);
            let (physical, images, unrelated) = observe_cell(&rows, &operation, &expected, ordinal);
            let work = daemon_work(&trace_rows(&compiler_trace, compiler_offset), &physical, &unrelated);
            if label == "warm-repeat" {
                for row in &rows {
                    let fields = &row["fields"];
                    if fields["message"] == "native group image association"
                        && expected.matches(fields, ordinal) && fields["disposition"] == "live_hit"
                    {
                        let key = (fields["image_registry"].as_u64().unwrap(),
                            fields["key_domain"].as_str().unwrap().to_owned(),
                            fields["image_entry"].as_u64().unwrap());
                        if let Some(prior) = prior_images.get(&key) {
                            assert_eq!(fields["image_instance"].as_u64(), Some(*prior));
                        }
                    }
                }
            }
            record_phase(&phases, json!({"phase":label, "operation":operation,
                "elapsed_ns":operation_elapsed_ns,
                "observer_elapsed_ns":observation_started.elapsed().as_nanos(), "expected_value":value,
                "expected_original":expected, "demanded_ordinal":ordinal,
                "compiler_requests":physical, "explicit_unrelated_issuers":unrelated, "observed_registry_keys":images.len(),
                "worker_work":work,
                "host_trace_start":offset, "host_trace_end":trace_offset(&host_trace),
                "compiler_trace_start":compiler_offset, "compiler_trace_end":trace_offset(&compiler_trace),
                "semantic_store_receipt_verified":true,
                "source_use_resolution_is_not_commit":true,
                "encoding_counts_are_demands_not_evaluations":true}));
            prior_images = images;
        }
        round.finish();
    })).await;
}

#[test]
#[should_panic(expected = "attribution UNKNOWN")]
fn compiler_request_without_its_execution_span_refuses_zero_work() {
    let fields = json!({"message":"compiler request identified", "transport":"daemon",
        "daemon_epoch":"a".repeat(64), "admission_id":1, "request_ordinal":1,
        "compile_request":"b".repeat(16)});
    // Diagnostic input only: this row cannot issue any compiler or actor work.
    attribute_requests(&[json!({"fields":fields})], "issued-cell");
}

#[test]
fn completed_request_census_preserves_absent_groups_and_refuses_missing_identification() {
    let request = json!({"daemon_epoch":"a".repeat(64), "admission_id":1,
        "request_ordinal":1, "compile_request":"b".repeat(16)});
    let rows = [
        "compiler request started",
        "compiler request finished",
        "compiler diagnostics processed",
    ]
    .map(|message| {
        json!({"span":request, "fields":{
            "message":message, "request_mode":"cell_plan", "exit_code":0}})
    });
    let work = daemon_work(&rows, std::slice::from_ref(&request), &[]);
    assert_eq!(work[0]["mode"], "cell_plan");
    assert_eq!(work[0]["group_observation_status"], "not_emitted");
    assert_eq!(work[0]["work"], json!([]));
    assert!(daemon_work(&[], &[], &[]).is_empty());
    let timing_only = [json!({"span":request, "fields":{
        "message":"compiler timing", "line":"tidepool-group-work {}"}})];
    assert!(std::panic::catch_unwind(|| daemon_work(&timing_only, &[], &[])).is_err());
    assert!(std::panic::catch_unwind(|| daemon_work(&rows, &[], &[])).is_err());
    assert!(std::panic::catch_unwind(|| daemon_work(
        &rows[..2],
        std::slice::from_ref(&request),
        &[]
    ))
    .is_err());
    assert!(std::panic::catch_unwind(|| daemon_work(
        &rows,
        &[request.clone(), request.clone()],
        &[]
    ))
    .is_err());
    let mut duplicate = rows.to_vec();
    duplicate.push(rows[0].clone());
    assert!(std::panic::catch_unwind(|| daemon_work(
        &duplicate,
        std::slice::from_ref(&request),
        &[]
    ))
    .is_err());
    let mut malformed = rows.to_vec();
    malformed[0]["span"]["request_ordinal"] = Value::Null;
    assert!(std::panic::catch_unwind(|| daemon_work(
        &malformed,
        std::slice::from_ref(&request),
        &[]
    ))
    .is_err());
    let unrelated = [json!({"physical_request":request, "issuer":{"context_call_id":"other"}})];
    let work = daemon_work(&rows, &[], &unrelated);
    assert_eq!(work[0]["attribution"], "explicit_unrelated");
}
