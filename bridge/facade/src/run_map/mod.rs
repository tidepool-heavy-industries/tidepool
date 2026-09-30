//! Read-only, bounded derivation of Exomonad run artifacts.
//! Missing evidence is not a negative observation or an acceptance verdict.
//!
//! Reviewing a live wave (`exomonad run-map <run-dir>`, add `--json` for machines):
//! 1. Run it with no window first. `tree` shows who exists, their model and effort,
//!    whether each has its own session (`inline` forks do not), and when each
//!    started, first called a tool, first and last replied, and its standing.
//! 2. Read `deliveries` next; it is current durable state, never windowed. An
//!    in-flight front row within the recovery grace period (twice the
//!    input-control deadline) renders `inbox=open (...)`, same as an ordinary
//!    delivery the host itself would not fence. Grep `inbox=fenced(`: the
//!    front row stayed with no native evidence (or `Unconfirmed`) past grace,
//!    or hit a terminal fence (`compacted`), and every row behind it waits.
//!    `host_input=no row` means Codex never admitted it.
//! 3. `notifications` pairs every send with its inbox row: `presented at refN`,
//!    or `not-presented` with the phase. Nothing is inferred from transcripts.
//! 4. `slowest calls` names the long hosted calls and splits their time; a high
//!    `wait` or checkout_wait share is checkout contention, not slow work.
//! 5. `rejections` and `nudges` count repeats per actor: the same rejection or
//!    after-tool annotation many times is a model stuck in a loop.
//! 6. On each later wake pass `--since 15m` (or the interval since the last
//!    wake) to see only new events; deliveries still show the whole inbox.
mod metadata;
mod review;
mod trace;
use metadata::{binding_thread, read_root, recorded_link};
pub use metadata::{
    ArtifactAvailability, ArtifactLayer, ArtifactProvenance, RecordedLink, RootBinding, TimeWindow,
    WatchState,
};
pub use review::{
    ActorDeliveries, CancellationRow, Deliveries, DeliveryRow, Fence, HostInput, MessagePhase,
    Notification, Observation, Percentiles, Provenance, Receipt, RepeatGroup, Review, Section,
    SlowCalls, ToolPercentiles, TreeNode, RECOVERY_GRACE_MS,
};
use serde::Serialize;
pub use trace::{
    ActorLifecycle, CallTiming, Cancellation, CorrelationCounts, DurationSummary, RunProvenance,
    TimelineEvent, TimelineFilter, TimingLink, TraceSummary,
};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "certainty", rename_all = "snake_case")]
pub enum Evidence<T> {
    Observed { value: T, source: String },
    Inferred { value: T, reason: String },
    Unknown { reason: String },
}

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

/// Parses a relative duration such as `90s`, `15m`, `2h` or `1d` into milliseconds.
pub fn parse_duration_ms(text: &str) -> Result<u64, String> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("duration {text:?} needs a unit: s, m, h or d"))?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("duration {text:?} needs a leading integer"))?;
    let scale = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err(format!("duration {text:?} needs a unit: ms, s, m, h or d")),
    };
    number
        .checked_mul(scale)
        .ok_or_else(|| format!("duration {text:?} overflows"))
}

/// Explicit resource bounds. A limit produces a diagnostic, not silent completeness.
#[derive(Clone, Copy)]
pub struct Limits {
    pub actors: usize,
    pub records_per_actor: usize,
    pub bytes_per_record: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            actors: 256,
            records_per_actor: 10_000,
            bytes_per_record: 1_048_576,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct ActorNode {
    pub actor: u64,
    pub incarnation: u64,
    pub provider_thread: Evidence<String>,
    pub events: Vec<RecordedEvent>,
    pub parent: Evidence<exomonad_actor::ActorRef>,
    pub source_seed: Evidence<String>,
    #[serde(skip)]
    directory: PathBuf,
    #[serde(skip)]
    rows: Vec<InboxRow>,
}

/// One inbox row as the review reads it: its receipt context, if tracked, and
/// the text prefix of a notification payload.
#[derive(Debug)]
struct InboxRow {
    sequence: u64,
    context: Option<Value>,
    text_prefix: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct RecordedEvent {
    pub sequence: Option<u64>,
    pub kind: EventKind,
    pub timestamp_unix_ms: Evidence<u64>,
    pub window_membership: Evidence<bool>,
    pub link: Evidence<RecordedLink>,
    pub source: String,
}
/// Inbox event labels classify records, never actor lifecycle success.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "label", rename_all = "snake_case")]
pub enum EventKind {
    /// A plain-text notification from another actor.
    Notification,
    SessionReady,
    WatchChanged,
    ChildExited,
    Other(String),
}
impl From<&str> for EventKind {
    fn from(value: &str) -> Self {
        match value {
            "notification" => Self::Notification,
            "sessionReady" => Self::SessionReady,
            "watchChanged" => Self::WatchChanged,
            "childExited" => Self::ChildExited,
            other => Self::Other(other.into()),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct RunMap {
    pub source: String,
    pub root: RootBinding,
    pub window: TimeWindow,
    pub actors: Vec<ActorNode>,
    pub diagnostics: Vec<String>,
    pub usage: Evidence<u64>,
    pub acceptance: Evidence<String>,
    pub provenance: RunProvenance,
    pub trace: TraceSummary,
    pub review: Review,
}

/// Partial artifact inventory. It deliberately does not parse assignment prose
/// or infer parentage, acceptance, failures or token usage from event labels.
pub fn read_run(run: &Path, limits: Limits) -> io::Result<RunMap> {
    read_windowed_run(run, limits, TimeWindow::default())
}

/// Timestamped events outside the window are omitted. Untimed events remain
/// explicitly unclassified; static actor directories are not dated by inference.
pub fn read_windowed_run(run: &Path, limits: Limits, window: TimeWindow) -> io::Result<RunMap> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        });
    read_observed_run(run, limits, window, &Observation::at(now))
}

/// [`read_windowed_run`] with the review measured at `observation`.
pub fn read_observed_run(
    run: &Path,
    limits: Limits,
    window: TimeWindow,
    observation: &Observation,
) -> io::Result<RunMap> {
    read_observed_run_filtered(run, limits, window, observation, &TimelineFilter::default())
}

/// Read a run and apply selectors to the bounded timeline projection only.
/// The actor inventory, review, and summary retain their existing semantics.
pub fn read_observed_run_filtered(
    run: &Path,
    limits: Limits,
    window: TimeWindow,
    observation: &Observation,
    timeline_filter: &TimelineFilter,
) -> io::Result<RunMap> {
    window.validate()?;
    let read_bound = u64::try_from(limits.bytes_per_record)
        .ok()
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "record byte limit must leave room for overflow detection",
            )
        })?;
    let (provenance, trace_path) = trace::provenance(run, read_bound);
    let mut report = RunMap {
        source: run.display().to_string(),
        root: read_root(run, read_bound),
        window,
        actors: Vec::new(),
        diagnostics: Vec::new(),
        usage: Evidence::Unknown {
            reason:
                "No read-only harness export of stored response usage is available to this reader"
                    .into(),
        },
        acceptance: Evidence::Unknown {
            reason: "No structured acceptance evidence consumed".into(),
        },
        provenance,
        trace: TraceSummary::default(),
        review: Review::placeholder(),
    };
    // Inspect the listing, retaining only the smallest keys. Selection is
    // independent of filesystem enumeration order and uses O(actor limit) memory.
    let mut directories = BTreeSet::new();
    let mut omitted = 0usize;
    for entry in fs::read_dir(run)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some((actor, incarnation)) = name.split_once('-') else {
            continue;
        };
        let (Ok(actor), Ok(incarnation)) = (actor.parse::<u64>(), incarnation.parse::<u64>())
        else {
            continue;
        };
        directories.insert((actor, incarnation, entry.path()));
        if directories.len() > limits.actors {
            directories.pop_last();
            omitted = omitted.saturating_add(1);
        }
    }
    if omitted != 0 {
        report.diagnostics.push(format!(
            "Actor directory limit reached; {omitted} directories omitted"
        ));
    }
    for (actor, incarnation, directory) in directories {
        let binding = directory.join("binding.json");
        let mut provider_thread = binding_thread(&binding, read_bound);
        if let Evidence::Observed {
            value: root_actor, ..
        } = &report.root.actor
        {
            if root_actor.id.0 == actor && root_actor.incarnation.0 == incarnation {
                provider_thread = report.root.actor_thread(provider_thread);
            }
        }
        let mut node = ActorNode {
            actor,
            incarnation,
            provider_thread,
            events: Vec::new(),
            parent: Evidence::Unknown {
                reason: "No structured admission parent artifact consumed".into(),
            },
            source_seed: Evidence::Unknown {
                reason: "No structured source seed artifact consumed".into(),
            },
            directory: directory.clone(),
            rows: Vec::new(),
        };
        let inbox = directory.join("inbox.jsonl");
        match File::open(&inbox) {
            Err(error) => report
                .diagnostics
                .push(format!("{}: {error}", inbox.display())),
            Ok(file) => {
                let mut reader = BufReader::new(file);
                for index in 0..=limits.records_per_actor {
                    if index == limits.records_per_actor {
                        match reader.fill_buf() {
                            Ok(bytes) if !bytes.is_empty() => report
                                .diagnostics
                                .push(format!("{}: record limit reached", inbox.display())),
                            Ok(_) => (),
                            Err(error) => report
                                .diagnostics
                                .push(format!("{}: inbox read failed: {error}", inbox.display())),
                        }
                        break;
                    }
                    let mut bytes = Vec::new();
                    let count = match reader
                        .by_ref()
                        .take(read_bound)
                        .read_until(b'\n', &mut bytes)
                    {
                        Ok(count) => count,
                        Err(error) => {
                            report.diagnostics.push(format!(
                                "{}:{}: inbox read failed: {error}",
                                inbox.display(),
                                index + 1
                            ));
                            break;
                        }
                    };
                    if count == 0 {
                        break;
                    }
                    let source = format!("{}:{}", inbox.display(), index + 1);
                    if count > limits.bytes_per_record {
                        report.diagnostics.push(format!(
                            "{source}: oversized record; remaining file not read"
                        ));
                        break;
                    }
                    if bytes.last() != Some(&b'\n') {
                        report
                            .diagnostics
                            .push(format!("{source}: incomplete tail ignored"));
                        break;
                    }
                    match serde_json::from_slice::<Value>(&bytes) {
                        Ok(value) => {
                            if let Some(sequence) = value["sequence"].as_u64() {
                                node.rows.push(InboxRow {
                                    sequence,
                                    context: value.get("receipt_context").cloned(),
                                    text_prefix: value["payload"].as_str().map(trace::text_prefix),
                                });
                            }
                            let kind = value["payload"]["type"]
                                .as_str()
                                .or(value["payload"].is_string().then_some("notification"));
                            if let Some(kind) = kind {
                                let timestamp = value["payload"]["occurred_at_unix_ms"].as_u64();
                                if timestamp.is_some_and(|time| !window.contains(time)) {
                                    continue;
                                }
                                let timestamp_unix_ms = match timestamp {
                                    Some(value) => Evidence::Observed {
                                        value,
                                        source: source.clone(),
                                    },
                                    None => Evidence::Unknown {
                                        reason: "Event has no recorded Unix-millisecond timestamp"
                                            .into(),
                                    },
                                };
                                let window_membership = if timestamp.is_some()
                                    || !window.is_bounded()
                                {
                                    Evidence::Observed {
                                        value: true,
                                        source: source.clone(),
                                    }
                                } else {
                                    Evidence::Unknown {
                                        reason: "Untimed event retained outside window accounting"
                                            .into(),
                                    }
                                };
                                let link = recorded_link(&value["payload"], &source);
                                node.events.push(RecordedEvent {
                                    timestamp_unix_ms,
                                    window_membership,
                                    link,
                                    sequence: value["sequence"].as_u64(),
                                    kind: kind.into(),
                                    source,
                                });
                            } else {
                                report
                                    .diagnostics
                                    .push(format!("{source}: missing event type"));
                            }
                        }
                        Err(_) => report
                            .diagnostics
                            .push(format!("{source}: invalid JSON record")),
                    }
                }
            }
        }
        report.actors.push(node);
    }
    if window.is_bounded() {
        let untimed = report
            .actors
            .iter()
            .flat_map(|actor| &actor.events)
            .filter(|event| matches!(event.window_membership, Evidence::Unknown { .. }))
            .count();
        if untimed > 0 {
            report.diagnostics.push(format!(
                "{untimed} untimed events retained with unknown window membership"
            ));
        }
    }
    let mut events = None;
    let mut trace_unavailable = String::new();
    if let Some(path) = trace_path {
        let (summary, read) = trace::read_trace(
            &path,
            limits,
            window,
            &mut report.diagnostics,
            timeline_filter,
        );
        report.trace = summary;
        if report.trace.unclassified_dispatch_failures > 0 {
            report.diagnostics.push(format!(
                "{} dispatch failures have no structured class; error prose was not classified",
                report.trace.unclassified_dispatch_failures
            ));
        }
        if read.omitted > 0 {
            report.diagnostics.push(format!(
                "{} review events omitted past the per-kind bound",
                read.omitted
            ));
        }
        if path.is_file() {
            events = Some(read);
        } else {
            trace_unavailable = format!("{} absent", path.display());
        }
    } else {
        trace_unavailable = "host trace location unknown: run status unavailable".into();
        report
            .diagnostics
            .push("Host trace location unknown because run status is unavailable".into());
    }
    report.review = review::build(
        run,
        &report.actors,
        events.as_ref(),
        &trace_unavailable,
        window,
        observation,
    );
    Ok(report)
}
impl RunMap {
    /// Perfetto JSON trace. Exported payloads contain metadata and source
    /// references only; source content must be retrieved separately.
    pub fn perfetto_trace(&self) -> serde_json::Value {
        let track_key = |event: &TimelineEvent| {
            if let Some(call_id) = &event.call_id {
                format!("{}:call:{call_id}", event.actor.as_deref().unwrap_or(""))
            } else if let Some(execution) = &event.execution {
                format!(
                    "{}:execution:{execution}",
                    event.actor.as_deref().unwrap_or("")
                )
            } else if let Some(actor) = &event.actor {
                format!("actor:{actor}")
            } else {
                format!("record:{}", event.source)
            }
        };
        let tracks = self
            .trace
            .timeline
            .iter()
            .map(track_key)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .enumerate()
            .map(|(index, key)| (key, u64::try_from(index + 1).unwrap_or(u64::MAX)))
            .collect::<BTreeMap<_, _>>();
        let events: Vec<serde_json::Value> = self.trace.timeline.iter().map(|event| {
            let mut args = serde_json::Map::new();
            args.insert("certainty".into(), serde_json::Value::String(event.certainty.into()));
            args.insert("source".into(), serde_json::Value::String(event.source.clone()));
            if let Some(value) = &event.actor { args.insert("actor".into(), value.clone().into()); }
            if let Some(value) = &event.execution { args.insert("execution".into(), value.clone().into()); }
            if let Some(value) = &event.call_id { args.insert("call_id".into(), value.clone().into()); }
            if let Some(value) = event.duration_ms { args.insert("duration_ms".into(), value.into()); }
            let interval = event.interval_start_unix_ms.zip(event.duration_ms);
            let mut row = serde_json::json!({
                "name": event.name, "cat": event.category,
                "ph": if interval.is_some() { "X" } else { "i" },
                "ts": interval.map_or(event.at_unix_ms, |(start, _)| start).saturating_mul(1000),
                "pid": 1,
                "tid": tracks[&track_key(event)], "args": args,
            });
            if let Some((_, duration)) = interval {
                row["dur"] = serde_json::json!(duration.saturating_mul(1000));
            } else {
                row["s"] = serde_json::json!("t");
            }
            row
        }).collect();
        serde_json::json!({
            "displayTimeUnit":"ms",
            "traceEvents": events,
            "metadata": { "run_map": {
                "from_unix_ms": self.window.from_unix_ms,
                "until_unix_ms": self.window.until_unix_ms,
                "omitted_timeline_events": self.trace.omitted_timeline_events,
                "timeline_filtered_events": self.trace.timeline_filtered_events,
                "timeline_outside_window_events": self.trace.timeline_outside_window_events,
                "timeline_unclassified_events": self.trace.timeline_unclassified_events,
                "diagnostic_count": self.diagnostics.len(),
                "source": self.source
            }}
        })
    }

    pub fn concise(&self) -> String {
        let mut output = format!("{}: {} observed actor directories, {} recorded events, {} diagnostics; usage and acceptance unknown (not peak concurrency)", self.source, self.actors.len(), self.actors.iter().map(|actor| actor.events.len()).sum::<usize>(), self.diagnostics.len());
        if let (Evidence::Observed { value: run_id, .. }, Evidence::Observed { value: model, .. }) =
            (&self.provenance.run_id, &self.provenance.model)
        {
            output.push_str(&format!("\n  run={run_id} configured model={model}"));
        }
        for actor in &self.actors {
            let thread = match &actor.provider_thread {
                Evidence::Observed { value, .. } => value.as_str(),
                _ => "unknown",
            };
            output.push_str(&format!(
                "\n  {}@{} thread={} events={} parent=unknown source=unknown",
                actor.actor,
                actor.incarnation,
                thread,
                actor.events.len()
            ));
        }
        if self.window.is_bounded() {
            output.push_str(&format!(
                "\n  UTC window {:?}..{:?} ms; untimed events remain unclassified",
                self.window.from_unix_ms, self.window.until_unix_ms
            ));
        }
        let tools: u64 = self
            .trace
            .host_tools
            .values()
            .map(|value| value.count)
            .sum();
        let rejected = self.trace.input_units.get("Rejected").copied().unwrap_or(0);
        output.push_str(&format!("\n  host tools={tools} compile requests={} prepared compiles={} Jev calls={} rejected units={rejected} dispatch failures classified={} unclassified={}", self.trace.compile_requests.count, self.trace.prepared_compiles.count, self.trace.jev_calls.count, self.trace.dispatch_failures.values().sum::<u64>(), self.trace.unclassified_dispatch_failures));
        for (tool, duration) in &self.trace.host_tools {
            output.push_str(&format!(
                "\n    {tool}: {} calls, recorded elapsed {} ms (overlapping)",
                duration.count, duration.total_ms
            ));
        }
        for (phase, duration) in &self.trace.phases {
            output.push_str(&format!(
                "\n    {phase}: {} recorded phases, elapsed {} ms (may overlap parent spans)",
                duration.count, duration.total_ms
            ));
        }
        for (class, count) in &self.trace.dispatch_failures {
            output.push_str(&format!("\n    dispatch failure {class}: {count}"));
        }
        if matches!(self.trace.typed_refusal_coverage, Evidence::Unknown { .. }) {
            output.push_str(
                "\n    typed refusal total unknown; no complete structured source consumed",
            );
        }
        for diagnostic in &self.diagnostics {
            output.push_str(&format!("\n    diagnostic: {diagnostic}"));
        }
        output.push_str(&self.review.concise());
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfetto_export_uses_completed_span_times_safe_names_and_distinct_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        let workspace = dir.path().join("workspace");
        let trace_path = workspace.join(".exomonad/logs/wave.jsonl");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(trace_path.parent().unwrap()).unwrap();
        fs::write(
            run.join("status.json"),
            serde_json::json!({
                "version": 4, "run_id": "wave", "workspace": workspace,
                "session": "session", "agent": {"model":"test", "effort":"low"},
                "phase": {"state":"ready", "root_actor":{"id":1,"incarnation":1}, "root_thread":"thread-1"}
            }).to_string(),
        ).unwrap();
        let base = 1_790_157_608_000_u64;
        let records = [
            serde_json::json!({"timestamp":"2026-09-23T10:00:08.500Z","target":"tidepool::host_dynamic_tools","fields":{"message":"close","actor":"1@1","execution":"exec-a","time.busy":"9s","time.idle":"1s"},"span":{"name":"tool_call","call_id":"call-a"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:09.500Z","target":"tidepool::host_dynamic_tools","fields":{"message":"close","actor":"1@1","execution":"exec-b","time.busy":"7s","time.idle":"0ms"},"span":{"name":"tool_call","call_id":"call-b"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:08.600Z","target":"exomonad_actor::resident_actor","fields":{"message":"jev call answered","elapsed_ms":"20"},"spans":[{"name":"cell","actor":"1@1","execution":"exec-a"}]}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:08.700Z","target":"exomonad_actor::resident_actor","fields":{"message":"jev call answered","elapsed_ms":"30"},"spans":[{"name":"cell","actor":"1@1","execution":"exec-b"}]}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:09.000Z","target":"exomonad_actor::call_timing","fields":{"message":"call timing","actor":1,"incarnation":1,"total_ms":700,"tool":"cell"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:09.100Z","target":"exomonad_actor::call_timing","fields":{"message":"call timing","actor":2,"incarnation":1,"total_ms":800,"tool":"cell"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:09.200Z","target":"exomonad_actor::resident_actor","fields":{"message":"actor notification sent","actor":"1@1","target":"2@1","reason":"SECRET REASON"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:09.300Z","target":"unknown::target","fields":{"message":"SECRET USER MESSAGE"}}),
            serde_json::json!({"timestamp":"2026-09-23T10:00:11.000Z","target":"exomonad_actor::call_timing","fields":{"message":"call timing","actor":1,"incarnation":1,"total_ms":1}}),
        ];
        fs::write(
            &trace_path,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let report = read_observed_run_filtered(
            &run,
            Limits::default(),
            TimeWindow {
                from_unix_ms: Some(base),
                until_unix_ms: Some(base + 2_000),
            },
            &Observation::at(base + 20_000),
            &TimelineFilter::default(),
        )
        .unwrap();
        let export = report.perfetto_trace();
        let events = export["traceEvents"].as_array().unwrap();
        let span = events
            .iter()
            .find(|event| event["name"] == "hosted tool call")
            .unwrap();
        assert_eq!(span["ph"], "X");
        assert_eq!(span["ts"], (base - 9_500) * 1_000);
        assert_eq!(span["dur"], 10_000_000);
        let call_tracks = events
            .iter()
            .filter(|event| event["name"] == "hosted tool call")
            .map(|event| event["tid"].as_u64().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(call_tracks.len(), 2);
        let execution_tracks = events
            .iter()
            .filter(|event| event["name"] == "Jev call answered")
            .map(|event| event["tid"].as_u64().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(execution_tracks.len(), 2);
        let instant = events
            .iter()
            .find(|event| event["name"] == "hosted call timing")
            .unwrap();
        assert_eq!(instant["ph"], "i");
        assert_eq!(instant["s"], "t");
        assert_eq!(instant["args"]["duration_ms"], 700);
        let notification = events
            .iter()
            .find(|event| event["name"] == "notification sent")
            .unwrap();
        assert!(!notification.to_string().contains("SECRET REASON"));
        assert!(!export.to_string().contains("SECRET USER MESSAGE"));
        let tracks = events
            .iter()
            .filter(|event| event["name"] == "hosted call timing")
            .map(|event| event["tid"].as_u64().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(tracks.len(), 2);
        let metadata = &export["metadata"]["run_map"];
        assert_eq!(metadata["timeline_filtered_events"], 0);
        assert_eq!(metadata["timeline_outside_window_events"], 1);
        assert_eq!(metadata["timeline_unclassified_events"], 1);
        assert!(metadata["diagnostic_count"].as_u64().is_some());
        let filtered = read_observed_run_filtered(
            &run,
            Limits::default(),
            TimeWindow {
                from_unix_ms: Some(base),
                until_unix_ms: Some(base + 2_000),
            },
            &Observation::at(base + 20_000),
            &TimelineFilter {
                actor: Some("1@1".into()),
                ..TimelineFilter::default()
            },
        )
        .unwrap()
        .perfetto_trace();
        assert_eq!(
            filtered["metadata"]["run_map"]["timeline_filtered_events"],
            1
        );
    }

    #[test]
    fn partial_map_preserves_unbound_actor_and_rejects_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let actor = dir.path().join("9-1");
        fs::create_dir(&actor).unwrap();
        fs::write(
            actor.join("inbox.jsonl"),
            b"{\"sequence\":1,\"payload\":{\"type\":\"childExited\"}}\n{\"payload\":",
        )
        .unwrap();
        let report = read_run(dir.path(), Limits::default()).unwrap();
        assert_eq!(report.actors.len(), 1);
        assert!(matches!(
            report.actors[0].provider_thread,
            Evidence::Unknown { .. }
        ));
        assert_eq!(report.actors[0].events.len(), 1);
        assert!(report
            .diagnostics
            .iter()
            .any(|entry| entry.contains("incomplete tail")));
        assert!(matches!(report.usage, Evidence::Unknown { .. }));
    }
    #[test]
    fn partial_map_bounds_oversized_input_and_directory_count() {
        let dir = tempfile::tempdir().unwrap();
        let actor = dir.path().join("1-1");
        fs::create_dir(&actor).unwrap();
        fs::write(actor.join("inbox.jsonl"), [b'x'; 100]).unwrap();
        let report = read_run(
            dir.path(),
            Limits {
                bytes_per_record: 16,
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(report.actors[0].events.is_empty());
        assert!(report.diagnostics[0].contains("oversized"));
        let report = read_run(
            dir.path(),
            Limits {
                actors: 0,
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(report.actors.is_empty());
        assert!(!report.diagnostics.is_empty());
    }

    #[test]
    fn partial_map_keeps_actors_after_local_read_failure() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("1-1/inbox.jsonl")).unwrap();
        fs::create_dir_all(dir.path().join("2-1")).unwrap();
        fs::write(
            dir.path().join("2-1/inbox.jsonl"),
            b"{\"sequence\":1,\"payload\":{\"type\":\"childExited\"}}\n",
        )
        .unwrap();
        for records_per_actor in [0, 10] {
            let report = read_run(
                dir.path(),
                Limits {
                    records_per_actor,
                    ..Limits::default()
                },
            )
            .unwrap();
            assert_eq!(report.actors.len(), 2);
            assert!(report.actors[0].events.is_empty());
            assert_eq!(
                report.actors[1].events.len(),
                usize::from(records_per_actor > 0)
            );
            assert!(report
                .diagnostics
                .iter()
                .any(|d| d.contains("1-1/inbox.jsonl")));
        }
        assert!(read_run(&dir.path().join("missing-root"), Limits::default()).is_err());
    }
    #[test]
    fn partial_map_actor_selection_is_creation_order_independent() {
        let mut selected = Vec::new();
        for order in [[9, 1, 5, 2], [2, 5, 1, 9]] {
            let dir = tempfile::tempdir().unwrap();
            for actor in order {
                fs::create_dir(dir.path().join(format!("{actor}-1"))).unwrap();
            }
            let report = read_run(
                dir.path(),
                Limits {
                    actors: 2,
                    ..Limits::default()
                },
            )
            .unwrap();
            selected.push(
                report
                    .actors
                    .iter()
                    .map(|a| (a.actor, a.incarnation))
                    .collect::<Vec<_>>(),
            );
            assert!(report
                .diagnostics
                .iter()
                .any(|d| d.contains("2 directories omitted")));
        }
        assert_eq!(selected[0], vec![(1, 1), (2, 1)]);
        assert_eq!(selected[0], selected[1]);
    }
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn partial_map_rejects_overflowing_byte_limit_at_entry() {
        let error = read_run(
            Path::new("not-read"),
            Limits {
                bytes_per_record: usize::MAX,
                ..Limits::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
