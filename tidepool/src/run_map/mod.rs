//! Read-only, bounded derivation of Shoal run artifacts.
//! Missing evidence is not a negative observation or an acceptance verdict.
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "certainty", rename_all = "snake_case")]
pub enum Evidence<T> {
    Observed { value: T, source: String },
    Inferred { value: T, reason: String },
    Unknown { reason: String },
}

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

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
}
#[derive(Debug, Serialize)]
pub struct RecordedEvent {
    pub sequence: Option<u64>,
    pub kind: EventKind,
    pub source: String,
}
/// Inbox event labels classify records, never actor lifecycle success.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "label", rename_all = "snake_case")]
pub enum EventKind {
    SessionReady,
    WatchChanged,
    ChildExited,
    Other(String),
}
impl From<&str> for EventKind {
    fn from(value: &str) -> Self {
        match value {
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
    pub actors: Vec<ActorNode>,
    pub diagnostics: Vec<String>,
    pub usage: Evidence<u64>,
    pub acceptance: Evidence<String>,
}

/// Partial artifact inventory. It deliberately does not parse assignment prose
/// or infer parentage, acceptance, failures or token usage from event labels.
pub fn read_run(run: &Path, limits: Limits) -> io::Result<RunMap> {
    let read_bound = u64::try_from(limits.bytes_per_record)
        .ok()
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "record byte limit must leave room for overflow detection",
            )
        })?;
    let mut report = RunMap {
        source: run.display().to_string(),
        actors: Vec::new(),
        diagnostics: Vec::new(),
        usage: Evidence::Unknown {
            reason: "Per-response usage reconciliation not implemented".into(),
        },
        acceptance: Evidence::Unknown {
            reason: "No structured acceptance evidence consumed".into(),
        },
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
        let thread = (|| -> Option<String> {
            let mut bytes = Vec::new();
            File::open(&binding)
                .ok()?
                .take(read_bound)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() > limits.bytes_per_record {
                return None;
            }
            serde_json::from_slice::<Value>(&bytes).ok()?["thread"]
                .as_str()
                .map(str::to_owned)
        })();
        let provider_thread = match thread {
            Some(value) => Evidence::Observed {
                value,
                source: binding.display().to_string(),
            },
            None => Evidence::Unknown {
                reason: format!(
                    "{} missing, unreadable, oversized or invalid",
                    binding.display()
                ),
            },
        };
        let mut node = ActorNode {
            actor,
            incarnation,
            provider_thread,
            events: Vec::new(),
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
                            if let Some(kind) = value["payload"]["type"].as_str() {
                                node.events.push(RecordedEvent {
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
    Ok(report)
}
impl RunMap {
    pub fn concise(&self) -> String {
        format!("{}: {} observed actor directories, {} recorded events, {} diagnostics; usage and acceptance unknown (not peak concurrency)", self.source, self.actors.len(), self.actors.iter().map(|actor| actor.events.len()).sum::<usize>(), self.diagnostics.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
