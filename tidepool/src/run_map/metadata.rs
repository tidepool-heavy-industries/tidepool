//! Bounded metadata projection; these observations never grant runtime custody.
use super::Evidence;
use crate::shoal::{RunPhase, RunStatus};
use serde::Serialize;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use tidepool_actor::ActorRef;

#[derive(Debug, Serialize)]
pub struct RootBinding {
    pub actor: Evidence<ActorRef>,
    pub provider_thread: Evidence<String>,
}

pub(super) fn binding_thread(path: &Path, read_bound: u64) -> Evidence<String> {
    let value = (|| -> Option<String> {
        let mut bytes = Vec::new();
        File::open(path)
            .ok()?
            .take(read_bound)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 == read_bound {
            return None;
        }
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()?["thread"]
            .as_str()
            .filter(|thread| !thread.is_empty())
            .map(str::to_owned)
    })();
    match value {
        Some(value) => Evidence::Observed {
            value,
            source: path.display().to_string(),
        },
        None => Evidence::Unknown {
            reason: format!(
                "{} missing, unreadable, oversized or invalid",
                path.display()
            ),
        },
    }
}

pub(super) fn read_root(run: &Path, read_bound: u64) -> RootBinding {
    let path = run.join("status.json");
    let status = (|| -> Option<RunStatus> {
        let mut bytes = Vec::new();
        File::open(&path)
            .ok()?
            .take(read_bound)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 == read_bound {
            return None;
        }
        crate::shoal::decode_run_status(&bytes).ok()
    })();
    let (root_actor, expected_thread) = match status.map(|status| status.phase) {
        Some(RunPhase::Ready {
            root_actor,
            root_thread,
        }) => (Some(root_actor), Some(root_thread.0)),
        Some(RunPhase::AwaitingBinding { root_actor }) => (Some(root_actor), None),
        _ => (None, None),
    };
    let actor = match root_actor {
        Some(value) => Evidence::Observed {
            value,
            source: path.display().to_string(),
        },
        None => Evidence::Unknown {
            reason: format!("{} does not establish root actor identity", path.display()),
        },
    };
    let mut provider_thread = binding_thread(&run.join("root-binding.json"), read_bound);
    if let (Some(expected), Evidence::Observed { value, .. }) = (expected_thread, &provider_thread)
    {
        if expected != *value {
            provider_thread = Evidence::Unknown {
                reason: "Conflicting status and root binding threads".into(),
            };
        }
    }
    RootBinding {
        actor,
        provider_thread,
    }
}

/// Bounds are UTC milliseconds since Unix epoch, inclusive start/exclusive end.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct TimeWindow {
    pub from_unix_ms: Option<u64>,
    pub until_unix_ms: Option<u64>,
}
impl TimeWindow {
    pub(super) fn validate(self) -> io::Result<()> {
        if matches!((self.from_unix_ms, self.until_unix_ms), (Some(from), Some(until)) if from > until)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "window start must not exceed end",
            ));
        }
        Ok(())
    }
    pub(super) fn contains(self, timestamp: u64) -> bool {
        self.from_unix_ms.is_none_or(|from| timestamp >= from)
            && self.until_unix_ms.is_none_or(|until| timestamp < until)
    }
    pub(super) fn is_bounded(self) -> bool {
        self.from_unix_ms.is_some() || self.until_unix_ms.is_some()
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedLink {
    /// A sessionReady envelope naming a request, not proof of a typed reply.
    RequestPresentation { request: u64 },
    /// A recorded dependency transition, not acceptance of the watched result.
    WatchTransition {
        owner: ActorRef,
        watch: u64,
        previous: WatchState,
        current: WatchState,
    },
}
#[derive(Debug, Serialize)]
#[serde(tag = "state", content = "label", rename_all = "snake_case")]
pub enum WatchState {
    Pending,
    Ready,
    Unavailable,
    Other(String),
}
impl From<&str> for WatchState {
    fn from(value: &str) -> Self {
        match value {
            "Pending" => Self::Pending,
            "Ready" => Self::Ready,
            "Unavailable" => Self::Unavailable,
            other => Self::Other(other.into()),
        }
    }
}

pub(super) fn recorded_link(payload: &serde_json::Value, source: &str) -> Evidence<RecordedLink> {
    let link = (|| match payload["type"].as_str()? {
        "sessionReady" => Some(RecordedLink::RequestPresentation {
            request: payload["request"].as_u64()?,
        }),
        "watchChanged" => Some(RecordedLink::WatchTransition {
            owner: serde_json::from_value(payload["owner"].clone()).ok()?,
            watch: payload["watch"].as_u64()?,
            previous: payload["previous"].as_str()?.into(),
            current: payload["current"].as_str()?.into(),
        }),
        _ => None,
    })();
    match link {
        Some(value) => Evidence::Observed {
            value,
            source: source.into(),
        },
        None => Evidence::Unknown {
            reason: "No supported structured edge in this event".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_map::{read_windowed_run, Limits};
    use serde_json::json;
    use std::fs;

    #[test]
    fn run_map_window_preserves_untimed_and_links_only_structured_edges() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("7-2")).unwrap();
        let rows = [
            json!({"sequence":1,"payload":{"type":"sessionReady","request":19,"message":"never emit this secret"}}),
            json!({"sequence":2,"payload":{"type":"watchChanged","owner":{"id":7,"incarnation":2},"watch":4,"previous":"Pending","current":"Ready","occurred_at_unix_ms":100}}),
            json!({"sequence":3,"payload":{"type":"watchChanged","occurred_at_unix_ms":200}}),
            json!({"sequence":4,"payload":{"type":"watchChanged","occurred_at_unix_ms":99}}),
        ];
        fs::write(
            dir.path().join("7-2/inbox.jsonl"),
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let report = read_windowed_run(
            dir.path(),
            Limits::default(),
            TimeWindow {
                from_unix_ms: Some(100),
                until_unix_ms: Some(200),
            },
        )
        .unwrap();
        let events = &report.actors[0].events;
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].window_membership,
            Evidence::Unknown { .. }
        ));
        assert!(matches!(
            events[0].link,
            Evidence::Observed {
                value: RecordedLink::RequestPresentation { request: 19 },
                ..
            }
        ));
        assert!(matches!(
            events[1].link,
            Evidence::Observed {
                value: RecordedLink::WatchTransition { watch: 4, .. },
                ..
            }
        ));
        assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
        assert!(read_windowed_run(
            dir.path(),
            Limits::default(),
            TimeWindow {
                from_unix_ms: Some(200),
                until_unix_ms: Some(100)
            }
        )
        .is_err());
    }

    #[test]
    fn run_map_root_uses_recorded_identity_and_rejects_conflicting_binding() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("7-2")).unwrap();
        fs::write(dir.path().join("status.json"), json!({
            "version":4,"run_id":"test","workspace":"/sanitized","session":"test",
            "agent":{"model":"test","effort":"low"},
            "phase":{"state":"ready","root_actor":{"id":7,"incarnation":2},"root_thread":"thread-root"}
        }).to_string()).unwrap();
        fs::write(
            dir.path().join("root-binding.json"),
            json!({"version":4,"thread":"thread-root"}).to_string(),
        )
        .unwrap();
        let report =
            read_windowed_run(dir.path(), Limits::default(), TimeWindow::default()).unwrap();
        assert!(
            matches!(&report.actors[0].provider_thread, Evidence::Observed { value, .. } if value == "thread-root")
        );
        fs::write(
            dir.path().join("7-2/binding.json"),
            json!({"version":4,"thread":"conflicting"}).to_string(),
        )
        .unwrap();
        let report =
            read_windowed_run(dir.path(), Limits::default(), TimeWindow::default()).unwrap();
        assert!(matches!(
            report.actors[0].provider_thread,
            Evidence::Unknown { .. }
        ));
        fs::write(
            dir.path().join("root-binding.json"),
            json!({"version":4,"thread":"status-conflict"}).to_string(),
        )
        .unwrap();
        let report =
            read_windowed_run(dir.path(), Limits::default(), TimeWindow::default()).unwrap();
        assert!(matches!(
            report.root.provider_thread,
            Evidence::Unknown { .. }
        ));
        fs::remove_file(dir.path().join("status.json")).unwrap();
        let report =
            read_windowed_run(dir.path(), Limits::default(), TimeWindow::default()).unwrap();
        assert!(matches!(report.root.actor, Evidence::Unknown { .. }));
        assert!(matches!(
            report.root.provider_thread,
            Evidence::Observed { .. }
        ));
    }
    #[test]
    fn run_map_root_rejects_unsupported_status_version_without_serde_fallback() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("7-2")).unwrap();
        fs::write(
            dir.path().join("root-binding.json"),
            json!({"version":4,"thread":"thread-root"}).to_string(),
        )
        .unwrap();
        let mut status = json!({
            "version":4,"run_id":"test","workspace":"/sanitized","session":"test",
            "agent":{"model":"test","effort":"low"},
            "phase":{"state":"ready","root_actor":{"id":7,"incarnation":2},"root_thread":"thread-root"}
        });
        for version in [0, 5, u32::MAX] {
            status["version"] = json!(version);
            // This shape would pass raw serde; only the owning decoder rejects it.
            assert!(serde_json::from_value::<RunStatus>(status.clone()).is_ok());
            fs::write(dir.path().join("status.json"), status.to_string()).unwrap();
            let report =
                read_windowed_run(dir.path(), Limits::default(), TimeWindow::default()).unwrap();
            assert!(matches!(report.root.actor, Evidence::Unknown { .. }));
            assert!(matches!(
                report.actors[0].provider_thread,
                Evidence::Unknown { .. }
            ));
            assert!(matches!(
                report.root.provider_thread,
                Evidence::Observed { .. }
            ));
        }
    }
}
