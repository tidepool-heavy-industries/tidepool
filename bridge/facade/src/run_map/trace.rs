//! Bounded projection of host trace metadata. Never retain tool input, output or errors.
use super::{Evidence, Limits, TimeWindow};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct RunProvenance {
    pub run_id: Evidence<String>,
    pub workspace: Evidence<String>,
    pub model: Evidence<String>,
    pub effort: Evidence<String>,
    pub host_generation: Evidence<u64>,
    pub trace: Evidence<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct DurationSummary {
    pub count: u64,
    pub total_ms: u64,
    pub min_ms: Option<u64>,
    pub max_ms: Option<u64>,
}
impl DurationSummary {
    fn add(&mut self, ms: u64) {
        self.count += 1;
        self.total_ms = self.total_ms.saturating_add(ms);
        self.min_ms = Some(self.min_ms.map_or(ms, |old| old.min(ms)));
        self.max_ms = Some(self.max_ms.map_or(ms, |old| old.max(ms)));
    }
}

#[derive(Debug, Serialize)]
pub struct TraceSummary {
    pub host_tools: BTreeMap<String, DurationSummary>,
    /// Named implementation phases. These overlap their parent tool spans.
    pub phases: BTreeMap<String, DurationSummary>,
    pub compile_requests: DurationSummary,
    pub prepared_compiles: DurationSummary,
    pub jev_calls: DurationSummary,
    pub input_units: BTreeMap<String, u64>,
    pub typed_refusals: BTreeMap<String, u64>,
    pub typed_refusal_coverage: Evidence<String>,
    pub dispatch_failures: BTreeMap<String, u64>,
    pub unclassified_dispatch_failures: u64,
    pub actor_lifecycle: BTreeMap<String, ActorLifecycle>,
    pub timing_correlations: CorrelationCounts,
    pub timing_links: Vec<TimingLink>,
    pub omitted_timing_links: u64,
}
#[derive(Debug, Serialize)]
pub struct TimingLink {
    pub kind: &'static str,
    pub id: String,
    pub elapsed_ms: u64,
    pub source: String,
}
impl Default for TraceSummary {
    fn default() -> Self {
        Self {
            host_tools: BTreeMap::new(),
            phases: BTreeMap::new(),
            compile_requests: DurationSummary::default(),
            prepared_compiles: DurationSummary::default(),
            jev_calls: DurationSummary::default(),
            input_units: BTreeMap::new(),
            typed_refusals: BTreeMap::new(),
            typed_refusal_coverage: unknown("No complete structured typed-refusal source consumed"),
            dispatch_failures: BTreeMap::new(),
            unclassified_dispatch_failures: 0,
            actor_lifecycle: BTreeMap::new(),
            timing_correlations: CorrelationCounts::default(),
            timing_links: Vec::new(),
            omitted_timing_links: 0,
        }
    }
}
#[derive(Debug, Default, Serialize)]
pub struct CorrelationCounts {
    pub host_call_id: u64,
    pub compile_request_id: u64,
    pub jev_execution_id: u64,
}
#[derive(Debug, Serialize)]
pub struct ActorLifecycle {
    pub launched: Evidence<String>,
    pub retired: Evidence<String>,
    pub parent: Evidence<String>,
    pub transcript: Evidence<String>,
}
impl Default for ActorLifecycle {
    fn default() -> Self {
        Self {
            launched: unknown("No matching structured launch record"),
            retired: unknown("No matching structured retirement record"),
            parent: unknown("No structured parent edge in consumed artifacts"),
            transcript: unknown("No authoritative transcript path in consumed artifacts"),
        }
    }
}

impl TraceSummary {
    fn link(&mut self, kind: &'static str, id: &str, elapsed_ms: u64, source: &str) {
        if id.is_empty() || id.len() > 128 || !id.is_ascii() {
            return;
        }
        if self.timing_links.len() < 256 {
            self.timing_links.push(TimingLink {
                kind,
                id: id.into(),
                elapsed_ms,
                source: source.into(),
            });
        } else {
            self.omitted_timing_links += 1;
        }
    }
}

fn unknown<T>(reason: &str) -> Evidence<T> {
    Evidence::Unknown {
        reason: reason.into(),
    }
}

fn read_status(run: &Path, bound: u64) -> Option<(crate::exomonad::RunStatus, String)> {
    let path = run.join("status.json");
    let mut bytes = Vec::new();
    File::open(&path)
        .ok()?
        .take(bound)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 >= bound {
        return None;
    }
    Some((
        crate::exomonad::decode_run_status(&bytes).ok()?,
        path.display().to_string(),
    ))
}

pub(super) fn provenance(run: &Path, bound: u64) -> (RunProvenance, Option<PathBuf>) {
    let status = read_status(run, bound);
    let trace = status.as_ref().map(|(status, _)| {
        status
            .workspace
            .join(".exomonad/logs")
            .join(format!("{}.jsonl", status.run_id))
    });
    let field = |value: Option<String>| match (&status, value) {
        (Some((_, source)), Some(value)) => Evidence::Observed {
            value,
            source: source.clone(),
        },
        _ => unknown("No valid bounded status.json evidence"),
    };
    let provenance = RunProvenance {
        run_id: field(status.as_ref().map(|(value, _)| value.run_id.clone())),
        workspace: field(
            status
                .as_ref()
                .map(|(value, _)| value.workspace.display().to_string()),
        ),
        model: field(status.as_ref().map(|(value, _)| value.agent.model.clone())),
        effort: field(
            status
                .as_ref()
                .map(|(value, _)| format!("{:?}", value.agent.effort)),
        ),
        host_generation: status.as_ref().map_or_else(
            || unknown("No valid bounded status.json evidence"),
            |(value, source)| Evidence::Observed {
                value: value.host_generation,
                source: source.clone(),
            },
        ),
        trace: trace.as_ref().map_or_else(
            || unknown("No validated trace location"),
            |path| {
                if path.is_file() {
                    Evidence::Observed {
                        value: path.display().to_string(),
                        source: run.join("status.json").display().to_string(),
                    }
                } else {
                    unknown("Host trace file absent at recorded workspace location")
                }
            },
        ),
    };
    (provenance, trace)
}

fn duration_ms(text: &str) -> Option<u64> {
    let split = text.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let number: f64 = text[..split].parse().ok()?;
    let multiplier = match &text[split..] {
        "s" => 1000.0,
        "ms" => 1.0,
        "µs" | "us" => 0.001,
        "ns" => 0.000_001,
        _ => return None,
    };
    let ms = number * multiplier;
    (ms.is_finite() && ms >= 0.0 && ms <= u64::MAX as f64).then(|| ms.round() as u64)
}

fn elapsed_ms(fields: &Value) -> Option<u64> {
    let busy = duration_ms(fields["time.busy"].as_str()?)?;
    let idle = duration_ms(fields["time.idle"].as_str()?)?;
    busy.checked_add(idle)
}

// ISO UTC timestamps emitted by tracing. Offset timestamps are deliberately unclassified.
fn timestamp_ms(value: &str) -> Option<u64> {
    let (date, clock) = value.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let clock = clock.strip_suffix('Z')?;
    let (h, rest) = clock.split_once(':')?;
    let (m, s) = rest.split_once(':')?;
    let (s, fraction) = s.split_once('.').unwrap_or((s, ""));
    let (h, m, s) = (
        h.parse::<i64>().ok()?,
        m.parse::<i64>().ok()?,
        s.parse::<i64>().ok()?,
    );
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || h > 23
        || m > 59
        || s > 60
        || h < 0
        || m < 0
        || s < 0
    {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let fraction = fraction.chars().take(3).collect::<String>();
    let millis = format!("{fraction:0<3}").parse::<i64>().ok()?;
    u64::try_from((days * 86400 + h * 3600 + m * 60 + s) * 1000 + millis).ok()
}

fn actor_key(raw: &str) -> Option<String> {
    if let Some((id, incarnation)) = raw.split_once('@') {
        return (id.parse::<u64>().is_ok() && incarnation.parse::<u64>().is_ok())
            .then(|| raw.into());
    }
    let id = raw
        .strip_prefix("ActorRef { id: ActorId(")?
        .split_once(')')?
        .0
        .parse::<u64>()
        .ok()?;
    let incarnation = raw
        .split("incarnation: Incarnation(")
        .nth(1)?
        .split_once(')')?
        .0
        .parse::<u64>()
        .ok()?;
    Some(format!("{id}@{incarnation}"))
}

fn class_label(raw: &str) -> Option<&str> {
    (!raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(raw)
}

fn known_phase(raw: &str) -> Option<&str> {
    matches!(
        raw,
        "git_capture_wait"
            | "git_capture"
            | "build_snapshot"
            | "compiler_request_admission"
            | "compiler_response"
            | "compiler_transaction_response"
            | "compiler_preflight"
            | "compiler_service"
            | "compiler_transaction_admission"
    )
    .then_some(raw)
}

pub(super) fn read_trace(
    path: &Path,
    limits: Limits,
    window: TimeWindow,
    diagnostics: &mut Vec<String>,
) -> TraceSummary {
    let mut summary = TraceSummary::default();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => {
            diagnostics.push(format!(
                "{}: host trace absent or unreadable",
                path.display()
            ));
            return summary;
        }
    };
    let mut reader = BufReader::new(file);
    let max_records = limits
        .records_per_actor
        .saturating_mul(limits.actors)
        .min(100_000);
    let bound = limits.bytes_per_record.saturating_add(1) as u64;
    for index in 0..max_records {
        let mut bytes = Vec::new();
        let count = match reader.by_ref().take(bound).read_until(b'\n', &mut bytes) {
            Ok(count) => count,
            Err(_) => {
                diagnostics.push(format!(
                    "{}:{}: trace read failed",
                    path.display(),
                    index + 1
                ));
                return summary;
            }
        };
        if count == 0 {
            return summary;
        }
        if count > limits.bytes_per_record {
            diagnostics.push(format!(
                "{}:{}: oversized trace record; remaining trace not read",
                path.display(),
                index + 1
            ));
            return summary;
        }
        if bytes.last() != Some(&b'\n') {
            diagnostics.push(format!(
                "{}:{}: incomplete trace tail ignored",
                path.display(),
                index + 1
            ));
            return summary;
        }
        let Ok(record) = serde_json::from_slice::<Value>(&bytes) else {
            diagnostics.push(format!(
                "{}:{}: invalid trace JSON",
                path.display(),
                index + 1
            ));
            continue;
        };
        let Some(time) = record["timestamp"].as_str().and_then(timestamp_ms) else {
            continue;
        };
        if !window.contains(time) {
            continue;
        }
        let source = format!("{}:{}", path.display(), index + 1);
        let fields = &record["fields"];
        let message = fields["message"].as_str().unwrap_or("");
        let target = record["target"].as_str().unwrap_or("");
        let span = &record["span"];
        let spans = record["spans"].as_array();
        if let (Some(phase), Some(ms)) = (
            fields["phase"].as_str().and_then(known_phase),
            fields["elapsed_ms"].as_u64(),
        ) {
            summary.phases.entry(phase.into()).or_default().add(ms);
            if let Some(id) = span["compile_request"].as_str().or_else(|| {
                spans.and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item["compile_request"].as_str())
                })
            }) {
                summary.link("phase", id, ms, &source);
            }
        }
        match (target, message) {
            ("tidepool::host_dynamic_tools", "close") if span["name"] == "tool_call" => {
                if let (Some(tool), Some(ms)) = (span["tool"].as_str(), elapsed_ms(fields)) {
                    summary.host_tools.entry(tool.into()).or_default().add(ms);
                    if let Some(id) = span["call_id"].as_str() {
                        summary.timing_correlations.host_call_id += 1;
                        summary.link("host_tool", id, ms, &source);
                    }
                }
            }
            ("tidepool_extract_cmd::endpoint", "close") if span["name"] == "compile_request" => {
                if let Some(ms) = elapsed_ms(fields) {
                    summary.compile_requests.add(ms);
                    if let Some(id) = span["compile_request"].as_str() {
                        summary.link("compile_request", id, ms, &source);
                    }
                }
                if span["compile_request"].as_str().is_some() {
                    summary.timing_correlations.compile_request_id += 1;
                }
            }
            ("tidepool_codegen::prepared_compile", "prepared compile") => {
                if let Some(ms) = fields["total_ms"].as_u64() {
                    summary.prepared_compiles.add(ms);
                }
            }
            ("exomonad_actor::resident_actor", "jev call answered") => {
                if let Some(ms) = fields["elapsed_ms"]
                    .as_u64()
                    .or_else(|| fields["elapsed_ms"].as_str().and_then(|s| s.parse().ok()))
                {
                    summary.jev_calls.add(ms);
                }
                if spans.is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item["execution"].as_str().is_some())
                }) {
                    summary.timing_correlations.jev_execution_id += 1;
                    if let (Some(id), Some(ms)) = (
                        spans.and_then(|items| {
                            items.iter().find_map(|item| item["execution"].as_str())
                        }),
                        fields["elapsed_ms"].as_u64().or_else(|| {
                            fields["elapsed_ms"]
                                .as_str()
                                .and_then(|value| value.parse().ok())
                        }),
                    ) {
                        summary.link("jev_execution", id, ms, &source);
                    }
                }
            }
            ("exomonad::content", "input unit receipt") => {
                if let Some(status) = fields["status"]
                    .as_str()
                    .filter(|value| matches!(*value, "Committed" | "Rejected" | "NotRun"))
                {
                    *summary.input_units.entry(status.into()).or_default() += 1;
                }
            }
            ("tidepool::host_dynamic_tools", "resident tool dispatch failed") => {
                if let Some(class) = fields["failure_class"].as_str().and_then(class_label) {
                    *summary.dispatch_failures.entry(class.into()).or_default() += 1;
                } else {
                    summary.unclassified_dispatch_failures += 1;
                }
            }
            (_, "typed refusal") => {
                if let Some(class) = fields["refusal_kind"].as_str().and_then(class_label) {
                    *summary.typed_refusals.entry(class.into()).or_default() += 1;
                }
            }
            (
                "tidepool::actor_host",
                "interactive application launched" | "interactive application retired",
            ) => {
                if let Some(actor) = fields["actor"].as_str().and_then(actor_key) {
                    let lifecycle = summary.actor_lifecycle.entry(actor).or_default();
                    let observation = Evidence::Observed {
                        value: record["timestamp"].as_str().unwrap_or_default().to_owned(),
                        source,
                    };
                    if message.ends_with("launched") {
                        lifecycle.launched = observation;
                    } else {
                        lifecycle.retired = observation;
                    }
                }
            }
            _ => (),
        }
    }
    diagnostics.push(format!("{}: trace record limit reached", path.display()));
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn structured_trace_counts_without_reading_output_or_error_prose() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");
        let records = [
            serde_json::json!({"timestamp":"2026-09-23T10:00:00.000Z","target":"tidepool::host_dynamic_tools","fields":{"message":"close","time.busy":"1.5s","time.idle":"500ms"},"span":{"name":"tool_call","tool":"haskell","call_id":"call-1"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:01.000Z","target":"tidepool_extract_cmd::endpoint","fields":{"message":"close","time.busy":"120ms","time.idle":"2ms"},"span":{"name":"compile_request","compile_request":"compile-1"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:02.000Z","target":"tidepool_codegen::prepared_compile","fields":{"message":"prepared compile","total_ms":91}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:03.000Z","target":"exomonad_actor::resident_actor","fields":{"message":"jev call answered","elapsed_ms":"73"},"spans":[{"name":"cell","execution":"exec-1"}]}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:04.000Z","target":"exomonad::content","fields":{"message":"input unit receipt","status":"Rejected","output":"secret ReplyUnauthorized"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:05.000Z","target":"tidepool::host_dynamic_tools","fields":{"message":"resident tool dispatch failed","error":"secret ReplyUnauthorized"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:06.000Z","target":"exomonad_actor::resident_actor","fields":{"message":"typed refusal","refusal_kind":"reply_unauthorized"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:07.000Z","target":"tidepool::actor_host","fields":{"message":"interactive application launched","actor":"ActorRef { id: ActorId(2), incarnation: Incarnation(1) }"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:08.000Z","target":"tidepool::host_dynamic_tools","fields":{"message":"close","time.busy":"10s","time.idle":"0ms"},"span":{"name":"tool_call","tool":"bash"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:07.500Z","target":"tidepool_extract_cmd::daemon","fields":{"message":"compiler request finished","phase":"compiler_service","elapsed_ms":830,"worker_source":"secret"},"span":{"name":"compile_request","compile_request":"compile-1"}}),
        ];
        fs::write(
            &path,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let mut diagnostics = Vec::new();
        let summary = read_trace(
            &path,
            Limits::default(),
            TimeWindow {
                from_unix_ms: Some(1790157600000),
                until_unix_ms: Some(1790157608000),
            },
            &mut diagnostics,
        );
        assert_eq!(summary.phases["compiler_service"].count, 1);
        assert_eq!(summary.phases["compiler_service"].total_ms, 830);
        assert_eq!(summary.host_tools["haskell"].total_ms, 2000);
        assert_eq!(summary.compile_requests.total_ms, 122);
        assert_eq!(summary.prepared_compiles.count, 1);
        assert_eq!(summary.jev_calls.count, 1);
        assert_eq!(summary.input_units["Rejected"], 1);
        assert_eq!(summary.typed_refusals["reply_unauthorized"], 1);
        assert_eq!(summary.unclassified_dispatch_failures, 1);
        assert_eq!(summary.timing_links.len(), 4);
        assert_eq!(summary.timing_links[0].id, "call-1");
        assert_eq!(summary.timing_links[1].id, "compile-1");
        assert_eq!(summary.timing_links[2].id, "exec-1");
        assert_eq!(summary.timing_links[3].id, "compile-1");
        assert!(summary.dispatch_failures.is_empty());
        assert!(summary.actor_lifecycle.contains_key("2@1"));
        assert!(!summary.host_tools.contains_key("bash"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn trace_limit_reports_incomplete_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");
        fs::write(
            &path,
            "{\"timestamp\":\"2026-09-23T10:00:00Z\"}\n{\"incomplete\":",
        )
        .unwrap();
        let mut diagnostics = Vec::new();
        let summary = read_trace(
            &path,
            Limits::default(),
            TimeWindow::default(),
            &mut diagnostics,
        );
        assert!(summary.host_tools.is_empty());
        assert!(diagnostics
            .iter()
            .any(|line| line.contains("incomplete trace tail")));
    }
}
