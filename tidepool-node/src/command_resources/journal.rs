use super::CommandResourceStatus;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tidepool_repr::jsonl::{SyncPolicy, TailPolicy};

const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum EventKind {
    Admission {
        producer: String,
        actor: String,
        command: String,
        requested_bytes: u64,
    },
    CancellationFence {
        producer: String,
        actor: String,
        command: String,
    },
    Allocation {
        producer: String,
        actor: String,
        command: String,
        requested_bytes: u64,
        allocation: String,
    },
    Started {
        producer: String,
        actor: String,
        command: String,
    },
    Terminal {
        producer: String,
        actor: String,
        command: String,
        disposition: CommandResourceStatus,
    },
    ProducerSealed {
        producer: String,
    },
    Acknowledged {
        producer: String,
        actor: String,
        command: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Row {
    version: u32,
    sequence: u64,
    #[serde(flatten)]
    event: EventKind,
}

pub(super) struct Journal {
    path: Option<PathBuf>,
    next_sequence: u64,
}

impl Journal {
    pub(super) fn ephemeral() -> Self {
        Self {
            path: None,
            next_sequence: 1,
        }
    }

    pub(super) fn open(path: PathBuf) -> std::io::Result<(Self, Vec<EventKind>)> {
        if let Some(parent) = path.parent() {
            tidepool_atomic_write::create_dir_all_durable(parent).map_err(std::io::Error::from)?;
        }
        let (rows, torn) = tidepool_repr::jsonl::read_tail(
            &path,
            |line| parse_row(line).map_err(|error| error.to_string()),
            TailPolicy::Repair,
        )
        .map_err(std::io::Error::other)?;
        if let Some(torn) = torn {
            tracing::warn!(path = %path.display(), line = torn.line_no, reason = %torn.reason,
                "repaired torn command ownership journal tail");
        }
        let mut expected = 1;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            if row.sequence != expected {
                return Err(std::io::Error::other(format!(
                    "command ownership journal sequence mismatch: expected {expected}, found {}",
                    row.sequence
                )));
            }
            expected += 1;
            events.push(row.event);
        }
        Ok((
            Self {
                path: Some(path),
                next_sequence: expected,
            },
            events,
        ))
    }

    pub(super) fn append(&mut self, event: EventKind) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let row = Row {
            version: VERSION,
            sequence: self.next_sequence,
            event,
        };
        let encoded = serde_json::to_string(&row).map_err(std::io::Error::other)?;
        // An append error is uncertain. Do not advance or retry it inside this
        // owner; callers retain the allocation and surface cleanup uncertainty.
        tidepool_repr::jsonl::append_new_line(path, &encoded, SyncPolicy::All)?;
        self.next_sequence += 1;
        Ok(())
    }
}

fn parse_row(line: &str) -> Result<Row, Box<dyn std::error::Error + Send + Sync>> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    let found = tidepool_repr::version_ladder::found_version(&value);
    let current =
        tidepool_repr::version_ladder::migrate_to_current(value, found, VERSION, VERSION, &[])?;
    let row: Row = serde_json::from_value(current)?;
    Ok(row)
}

pub(super) fn allocation_path(root: &Path, allocation: &str) -> std::io::Result<PathBuf> {
    let path = Path::new(allocation);
    if path.is_absolute()
        || path.components().count() != 2
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(std::io::Error::other("invalid journal allocation identity"));
    }
    Ok(root.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_rows_reopen_in_order_and_repair_only_a_torn_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ownership.jsonl");
        let (mut journal, events) = Journal::open(path.clone()).unwrap();
        assert!(events.is_empty());
        journal
            .append(EventKind::Admission {
                producer: "run-a".into(),
                actor: "actor-1".into(),
                command: "command-2".into(),
                requested_bytes: 4096,
            })
            .unwrap();
        journal
            .append(EventKind::ProducerSealed {
                producer: "run-a".into(),
            })
            .unwrap();
        drop(journal);

        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"version\":1")
            .unwrap();
        let (mut reopened, events) = Journal::open(path.clone()).unwrap();
        assert_eq!(events.len(), 2);
        reopened
            .append(EventKind::Acknowledged {
                producer: "run-a".into(),
                actor: "actor-1".into(),
                command: "command-2".into(),
            })
            .unwrap();
        drop(reopened);
        let (_, events) = Journal::open(path).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn allocation_identity_cannot_escape_the_delegated_tree() {
        let root = Path::new("/commands");
        assert_eq!(
            allocation_path(root, "actor-1/command-2").unwrap(),
            root.join("actor-1/command-2")
        );
        assert!(allocation_path(root, "../outside").is_err());
        assert!(allocation_path(root, "/outside/command").is_err());
        assert!(allocation_path(root, "too/many/components").is_err());
    }
}
