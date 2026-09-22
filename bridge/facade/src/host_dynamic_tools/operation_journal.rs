use super::{CallRequest, CallResponse};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use tidepool_repr::jsonl::{SyncPolicy, TailPolicy};

const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationKey {
    thread_id: String,
    turn_id: String,
    call_id: String,
    context_call_id: Option<String>,
}

impl From<&CallRequest> for OperationKey {
    fn from(request: &CallRequest) -> Self {
        Self {
            thread_id: request.thread_id.clone(),
            turn_id: request.turn_id.clone(),
            call_id: request.call_id.clone(),
            context_call_id: request.context_call_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BoundaryKey {
    pub thread_id: String,
    pub context_call_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Created,
    Accepted {
        key: OperationKey,
        request_digest: String,
    },
    Terminal {
        key: OperationKey,
        request_digest: String,
        response: CallResponse,
    },
    BoundarySettled {
        boundary: BoundaryKey,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Row {
    version: u32,
    sequence: u64,
    #[serde(flatten)]
    event: Event,
}

struct Record {
    request_digest: String,
    response: Option<CallResponse>,
}

pub(super) enum Admission {
    New,
    Known(CallResponse),
    Uncertain,
}

pub(super) struct OperationJournal {
    path: PathBuf,
    next_sequence: u64,
    records: BTreeMap<OperationKey, Record>,
    settled_boundaries: BTreeSet<BoundaryKey>,
    created: bool,
    uncertain: bool,
}

impl OperationJournal {
    pub(super) fn open(path: PathBuf) -> std::io::Result<Self> {
        Self::open_with_mode(path, false)
    }

    pub(super) fn open_existing(path: PathBuf) -> std::io::Result<Self> {
        Self::open_with_mode(path, true)
    }

    fn open_with_mode(path: PathBuf, require_existing: bool) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            tidepool_atomic_write::create_dir_all_durable(parent).map_err(std::io::Error::from)?;
        }
        let existed = path.try_exists()?;
        if require_existing && !existed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "resumed conversation has no hosted-operation ownership journal",
            ));
        }
        let (rows, torn) = tidepool_repr::jsonl::read_tail(
            &path,
            |line| parse_row(line).map_err(|error| error.to_string()),
            TailPolicy::Repair,
        )
        .map_err(std::io::Error::other)?;
        if let Some(torn) = torn {
            tracing::warn!(path = %path.display(), line = torn.line_no, reason = %torn.reason,
                "repaired torn hosted-operation journal tail");
        }
        let mut journal = Self {
            path,
            next_sequence: 1,
            records: BTreeMap::new(),
            settled_boundaries: BTreeSet::new(),
            created: false,
            uncertain: false,
        };
        for row in rows {
            if row.sequence != journal.next_sequence {
                return Err(std::io::Error::other(format!(
                    "hosted-operation journal sequence mismatch: expected {}, found {}",
                    journal.next_sequence, row.sequence
                )));
            }
            journal.next_sequence += 1;
            journal.apply(row.event)?;
        }
        if existed && !journal.created {
            return Err(std::io::Error::other(
                "hosted-operation journal lacks a durable creation marker",
            ));
        }
        if !existed {
            journal.append(Event::Created)?;
            journal.created = true;
        }
        Ok(journal)
    }

    pub(super) fn settled_boundaries(&self) -> impl Iterator<Item = &BoundaryKey> {
        self.settled_boundaries.iter()
    }

    pub(super) fn uncertain_boundaries(&self) -> impl Iterator<Item = BoundaryKey> + '_ {
        self.records
            .iter()
            .filter(|(_, record)| record.response.is_none())
            .filter_map(|(key, _)| {
                key.context_call_id
                    .as_ref()
                    .map(|context_call_id| BoundaryKey {
                        thread_id: key.thread_id.clone(),
                        context_call_id: context_call_id.clone(),
                    })
            })
            .filter(|boundary| !self.settled_boundaries.contains(boundary))
    }

    pub(super) fn admit(&mut self, request: &CallRequest) -> std::io::Result<Admission> {
        let key = OperationKey::from(request);
        let request_digest = digest(request)?;
        if let Some(record) = self.records.get(&key) {
            if record.request_digest != request_digest {
                return Err(std::io::Error::other(
                    "hosted operation identity was reused with different input",
                ));
            }
            return Ok(record
                .response
                .clone()
                .map_or(Admission::Uncertain, Admission::Known));
        }
        self.append(Event::Accepted {
            key: key.clone(),
            request_digest: request_digest.clone(),
        })?;
        self.records.insert(
            key,
            Record {
                request_digest,
                response: None,
            },
        );
        Ok(Admission::New)
    }

    pub(super) fn finish(
        &mut self,
        request: &CallRequest,
        response: &CallResponse,
    ) -> std::io::Result<()> {
        let key = OperationKey::from(request);
        let request_digest = digest(request)?;
        let record = self
            .records
            .get(&key)
            .ok_or_else(|| std::io::Error::other("hosted operation finished before acceptance"))?;
        if record.request_digest != request_digest {
            return Err(std::io::Error::other(
                "hosted operation identity was reused with different input",
            ));
        }
        if let Some(existing) = &record.response {
            return if existing == response {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    "hosted operation has conflicting terminal outcomes",
                ))
            };
        }
        self.append(Event::Terminal {
            key: key.clone(),
            request_digest,
            response: response.clone(),
        })?;
        self.records
            .get_mut(&key)
            .ok_or_else(|| {
                std::io::Error::other("hosted operation disappeared after durable outcome")
            })?
            .response = Some(response.clone());
        Ok(())
    }

    pub(super) fn settle_boundary(&mut self, boundary: BoundaryKey) -> std::io::Result<()> {
        if self.settled_boundaries.contains(&boundary) {
            return Ok(());
        }
        self.append(Event::BoundarySettled {
            boundary: boundary.clone(),
        })?;
        self.settled_boundaries.insert(boundary);
        Ok(())
    }

    fn apply(&mut self, event: Event) -> std::io::Result<()> {
        match event {
            Event::Created => {
                if self.created {
                    return Err(std::io::Error::other(
                        "duplicate hosted-operation journal creation marker",
                    ));
                }
                self.created = true;
            }
            Event::Accepted {
                key,
                request_digest,
            } => {
                if self
                    .records
                    .insert(
                        key,
                        Record {
                            request_digest,
                            response: None,
                        },
                    )
                    .is_some()
                {
                    return Err(std::io::Error::other(
                        "duplicate hosted-operation acceptance",
                    ));
                }
            }
            Event::Terminal {
                key,
                request_digest,
                response,
            } => {
                let record = self.records.get_mut(&key).ok_or_else(|| {
                    std::io::Error::other("hosted-operation outcome precedes acceptance")
                })?;
                if record.request_digest != request_digest || record.response.is_some() {
                    return Err(std::io::Error::other(
                        "hosted-operation outcome conflicts with durable acceptance",
                    ));
                }
                record.response = Some(response);
            }
            Event::BoundarySettled { boundary } => {
                if !self.settled_boundaries.insert(boundary) {
                    return Err(std::io::Error::other(
                        "duplicate hosted-operation boundary settlement",
                    ));
                }
            }
        }
        Ok(())
    }

    fn append(&mut self, event: Event) -> std::io::Result<()> {
        if self.uncertain {
            return Err(std::io::Error::other(
                "hosted-operation journal has an uncertain append; reopen it exclusively",
            ));
        }
        let row = Row {
            version: VERSION,
            sequence: self.next_sequence,
            event,
        };
        let encoded = serde_json::to_string(&row).map_err(std::io::Error::other)?;
        if let Err(error) =
            tidepool_repr::jsonl::append_new_line(&self.path, &encoded, SyncPolicy::All)
        {
            self.uncertain = true;
            return Err(error);
        }
        self.next_sequence += 1;
        Ok(())
    }
}

fn digest(request: &CallRequest) -> std::io::Result<String> {
    let encoded = serde_json::to_vec(request).map_err(std::io::Error::other)?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn parse_row(line: &str) -> Result<Row, Box<dyn std::error::Error + Send + Sync>> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    let found = tidepool_repr::version_ladder::found_version(&value);
    let current =
        tidepool_repr::version_ladder::migrate_to_current(value, found, VERSION, VERSION, &[])?;
    Ok(serde_json::from_value(current)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(source: &str) -> CallRequest {
        CallRequest {
            context_call_id: Some("outer-call".into()),
            protocol_version: super::super::PROTOCOL_VERSION,
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            call_id: "call".into(),
            namespace: None,
            tool: "haskell".into(),
            arguments: serde_json::Value::String(source.into()),
        }
    }

    #[test]
    fn restart_returns_known_or_uncertain_without_redispatch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.jsonl");
        let mut journal = OperationJournal::open(path.clone()).unwrap();
        assert!(matches!(
            journal.admit(&request("effect-a")).unwrap(),
            Admission::New
        ));
        drop(journal);

        let mut journal = OperationJournal::open(path.clone()).unwrap();
        assert!(matches!(
            journal.admit(&request("effect-a")).unwrap(),
            Admission::Uncertain
        ));
        assert!(journal.admit(&request("effect-b")).is_err());
        let response = CallResponse::text("retained result".into());
        journal.finish(&request("effect-a"), &response).unwrap();
        drop(journal);

        let mut journal = OperationJournal::open(path).unwrap();
        let Admission::Known(recovered) = journal.admit(&request("effect-a")).unwrap() else {
            panic!("terminal outcome was not recovered")
        };
        assert_eq!(recovered, response);
    }

    #[test]
    fn settled_boundaries_survive_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.jsonl");
        let boundary = BoundaryKey {
            thread_id: "thread".into(),
            context_call_id: "outer-call".into(),
        };
        let mut journal = OperationJournal::open(path.clone()).unwrap();
        journal.settle_boundary(boundary.clone()).unwrap();
        drop(journal);
        let journal = OperationJournal::open(path).unwrap();
        assert_eq!(
            journal.settled_boundaries().collect::<Vec<_>>(),
            vec![&boundary]
        );
    }

    #[test]
    fn accepted_operation_fences_its_unsettled_boundary_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.jsonl");
        let mut journal = OperationJournal::open(path.clone()).unwrap();
        assert!(matches!(
            journal.admit(&request("effect-a")).unwrap(),
            Admission::New
        ));
        drop(journal);

        let journal = OperationJournal::open_existing(path).unwrap();
        assert_eq!(
            journal.uncertain_boundaries().collect::<Vec<_>>(),
            vec![BoundaryKey {
                thread_id: "thread".into(),
                context_call_id: "outer-call".into(),
            }]
        );
    }

    #[test]
    fn resumed_conversation_requires_a_durable_creation_marker() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.jsonl");
        assert_eq!(
            OperationJournal::open_existing(missing)
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::NotFound
        );

        let empty = directory.path().join("empty.jsonl");
        std::fs::write(&empty, b"").unwrap();
        assert!(OperationJournal::open_existing(empty)
            .err()
            .unwrap()
            .to_string()
            .contains("creation marker"));

        let initialized = directory.path().join("initialized.jsonl");
        drop(OperationJournal::open(initialized.clone()).unwrap());
        OperationJournal::open_existing(initialized).unwrap();
    }
}
