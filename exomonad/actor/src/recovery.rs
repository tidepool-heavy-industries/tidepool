//! Durable actor identity and lifecycle evidence for host reconstruction.
//!
//! This journal records only Rust-owned facts. Live Haskell values, mailbox
//! payloads, continuations, requests, and watches remain intentionally absent:
//! a restarted host reports those as lost instead of pretending to serialize
//! the resident heap.

use crate::{ActorDescriptor, ActorExitKind, ActorRef};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tidepool_repr::jsonl::{SyncPolicy, TailPolicy};

// Version 2 adds the creation marker that distinguishes a newly initialized
// owner from missing recovery evidence. Version 1 is rejected rather than
// silently treating an old or lost journal as an empty current run.
const VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableActorAdmission {
    pub actor: ActorRef,
    pub label: String,
    pub creator: Option<ActorRef>,
    pub supervisor_parent: Option<ActorRef>,
    pub context_parent: Option<ActorRef>,
    pub actor_path: Option<String>,
    pub role: String,
    #[serde(default)]
    pub descendant_depth: u16,
    #[serde(default)]
    pub descendant_active_children: Option<u16>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub instructions: Option<String>,
    pub launch_worktrees: Vec<String>,
    pub source_layer: Vec<PathBuf>,
}

impl DurableActorAdmission {
    fn capture(actor: ActorRef, descriptor: &ActorDescriptor, launch_worktrees: &[String]) -> Self {
        let descendants = descriptor.effective_role().descendants();
        Self {
            actor,
            label: descriptor.label().to_owned(),
            creator: descriptor.creator(),
            supervisor_parent: descriptor.supervisor_parent(),
            context_parent: descriptor.context_parent(),
            actor_path: descriptor.actor_path().map(ToString::to_string),
            role: format!("{:?}", descriptor.effective_role().role()).to_ascii_lowercase(),
            descendant_depth: descendants.maximum_depth,
            descendant_active_children: descendants.maximum_active_children,
            model: descriptor.model_name().map(str::to_owned),
            effort: descriptor
                .fork_effort()
                .map(|effort| format!("{effort:?}").to_ascii_lowercase()),
            instructions: descriptor.instructions().map(str::to_owned),
            launch_worktrees: launch_worktrees.to_vec(),
            source_layer: descriptor.source_layer().to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableActorTerminal {
    pub kind: ActorExitKind,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableActorRecord {
    pub admission: DurableActorAdmission,
    pub application: Option<DurableActorApplication>,
    pub terminal: Option<DurableActorTerminal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableActorApplication {
    pub binding_path: PathBuf,
    pub conversation: Option<String>,
    pub accepted_source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum EventKind {
    Created,
    Admitted {
        admission: Box<DurableActorAdmission>,
    },
    ApplicationPrepared {
        actor: ActorRef,
        binding_path: PathBuf,
        #[serde(default)]
        accepted_source: Option<String>,
    },
    ApplicationBound {
        actor: ActorRef,
        conversation: String,
    },
    Retired {
        actor: ActorRef,
        terminal: DurableActorTerminal,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Row {
    version: u32,
    sequence: u64,
    #[serde(flatten)]
    event: EventKind,
}

struct State {
    next_sequence: u64,
    records: BTreeMap<ActorRef, DurableActorRecord>,
    uncertain: bool,
}

/// Single-owner durable lifecycle writer for one run.
pub struct ActorRecoveryJournal {
    path: PathBuf,
    state: Mutex<State>,
}

impl ActorRecoveryJournal {
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Arc<Self>> {
        Self::open_with_mode(path.into(), false)
    }

    /// Reopen lifecycle evidence for a later host incarnation.
    ///
    /// Recovery must not silently replace a lost journal with an empty owner:
    /// that would admit a fresh root while the prior actor identities and
    /// external resources remain unaccounted for.
    pub fn open_existing(path: impl Into<PathBuf>) -> std::io::Result<Arc<Self>> {
        Self::open_with_mode(path.into(), true)
    }

    fn open_with_mode(path: PathBuf, require_existing: bool) -> std::io::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            tidepool_atomic_write::create_dir_all_durable(parent).map_err(std::io::Error::from)?;
        }
        let existed = path.try_exists()?;
        if require_existing && !existed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "recovered host has no actor lifecycle journal",
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
                "repaired torn actor lifecycle journal tail");
        }
        let (records, expected, created) = replay(rows)?;
        if existed && !created {
            return Err(std::io::Error::other(
                "actor lifecycle journal lacks a durable creation marker",
            ));
        }
        let journal = Arc::new(Self {
            path,
            state: Mutex::new(State {
                next_sequence: expected,
                records,
                uncertain: false,
            }),
        });
        if !existed {
            let mut state = journal.state.lock();
            journal.append(&mut state, EventKind::Created)?;
        }
        Ok(journal)
    }

    /// Replays the journal at `path` for an observer of a live run: the file is
    /// neither created nor repaired, and a torn final row is left in place and
    /// excluded from the returned records.
    pub fn read_observed(path: &std::path::Path) -> std::io::Result<Vec<DurableActorRecord>> {
        let (rows, _torn) = tidepool_repr::jsonl::read_tail(
            path,
            |line| parse_row(line).map_err(|error| error.to_string()),
            TailPolicy::Observe,
        )
        .map_err(std::io::Error::other)?;
        Ok(replay(rows)?.0.into_values().collect())
    }

    pub fn records(&self) -> Vec<DurableActorRecord> {
        self.state.lock().records.values().cloned().collect()
    }

    pub(crate) fn admit(
        &self,
        actor: ActorRef,
        descriptor: &ActorDescriptor,
        launch_worktrees: &[String],
    ) -> std::io::Result<()> {
        let admission = DurableActorAdmission::capture(actor, descriptor, launch_worktrees);
        let mut state = self.state.lock();
        ensure_writable(&state)?;
        match state.records.get(&actor) {
            Some(existing) if existing.admission == admission && existing.terminal.is_none() => {
                return Ok(())
            }
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor identity {actor} was reused with different durable parameters"
                )))
            }
            None => {}
        }
        self.append(
            &mut state,
            EventKind::Admitted {
                admission: Box::new(admission.clone()),
            },
        )?;
        state.records.insert(
            actor,
            DurableActorRecord {
                admission,
                application: None,
                terminal: None,
            },
        );
        Ok(())
    }

    pub(crate) fn retire(
        &self,
        actor: ActorRef,
        kind: ActorExitKind,
        summary: String,
    ) -> std::io::Result<()> {
        let terminal = DurableActorTerminal { kind, summary };
        let mut state = self.state.lock();
        ensure_writable(&state)?;
        let record = state.records.get(&actor).ok_or_else(|| {
            std::io::Error::other(format!("actor {actor} retired without durable admission"))
        })?;
        match &record.terminal {
            Some(existing) if existing == &terminal => return Ok(()),
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor {actor} retired with a conflicting terminal disposition"
                )))
            }
            None => {}
        }
        self.append(
            &mut state,
            EventKind::Retired {
                actor,
                terminal: terminal.clone(),
            },
        )?;
        state
            .records
            .get_mut(&actor)
            .ok_or_else(|| std::io::Error::other("actor admission disappeared during retirement"))?
            .terminal = Some(terminal);
        Ok(())
    }

    pub fn prepare_application(
        &self,
        actor: ActorRef,
        binding_path: PathBuf,
        accepted_source: Option<String>,
    ) -> std::io::Result<()> {
        let mut state = self.state.lock();
        ensure_writable(&state)?;
        let record = state.records.get(&actor).ok_or_else(|| {
            std::io::Error::other(format!(
                "application for actor {actor} has no durable admission"
            ))
        })?;
        match &record.application {
            Some(existing)
                if existing.binding_path == binding_path
                    && existing.conversation.is_none()
                    && existing.accepted_source == accepted_source =>
            {
                return Ok(())
            }
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor {actor} reused its application identity with changed parameters"
                )))
            }
            None => {}
        }
        self.append(
            &mut state,
            EventKind::ApplicationPrepared {
                actor,
                binding_path: binding_path.clone(),
                accepted_source: accepted_source.clone(),
            },
        )?;
        state
            .records
            .get_mut(&actor)
            .ok_or_else(|| {
                std::io::Error::other("actor admission disappeared during application preparation")
            })?
            .application = Some(DurableActorApplication {
            binding_path,
            conversation: None,
            accepted_source,
        });
        Ok(())
    }

    pub fn bind_application(&self, actor: ActorRef, conversation: String) -> std::io::Result<()> {
        let mut state = self.state.lock();
        ensure_writable(&state)?;
        let application = state
            .records
            .get(&actor)
            .and_then(|record| record.application.as_ref())
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "binding for actor {actor} precedes durable application preparation"
                ))
            })?;
        match &application.conversation {
            Some(existing) if existing == &conversation => return Ok(()),
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor {actor} published conflicting conversation identities"
                )))
            }
            None => {}
        }
        self.append(
            &mut state,
            EventKind::ApplicationBound {
                actor,
                conversation: conversation.clone(),
            },
        )?;
        state
            .records
            .get_mut(&actor)
            .and_then(|record| record.application.as_mut())
            .ok_or_else(|| {
                std::io::Error::other("application disappeared during binding publication")
            })?
            .conversation = Some(conversation);
        Ok(())
    }

    fn append(&self, state: &mut State, event: EventKind) -> std::io::Result<()> {
        let row = Row {
            version: VERSION,
            sequence: state.next_sequence,
            event,
        };
        let encoded = serde_json::to_string(&row).map_err(std::io::Error::other)?;
        if let Err(error) =
            tidepool_repr::jsonl::append_new_line(&self.path, &encoded, SyncPolicy::All)
        {
            // The row may be visible. This handle cannot safely append again;
            // restart and exclusive replay are the only reconciliation path.
            state.uncertain = true;
            return Err(error);
        }
        state.next_sequence += 1;
        Ok(())
    }
}

fn ensure_writable(state: &State) -> std::io::Result<()> {
    if state.uncertain {
        Err(std::io::Error::other(
            "actor lifecycle journal has an uncertain append; reopen it exclusively",
        ))
    } else {
        Ok(())
    }
}

type Replayed = (BTreeMap<ActorRef, DurableActorRecord>, u64, bool);

/// Validates row order and the creation marker, returning the records, the
/// next sequence, and whether the creation marker was present.
fn replay(rows: Vec<Row>) -> std::io::Result<Replayed> {
    let mut expected = 1;
    let mut records = BTreeMap::new();
    let mut created = false;
    for row in rows {
        if row.sequence != expected {
            return Err(std::io::Error::other(format!(
                "actor lifecycle journal sequence mismatch: expected {expected}, found {}",
                row.sequence
            )));
        }
        expected += 1;
        match row.event {
            EventKind::Created if !created => created = true,
            EventKind::Created => {
                return Err(std::io::Error::other(
                    "duplicate actor lifecycle journal creation marker",
                ));
            }
            event if created => apply_event(&mut records, event)?,
            _ => {
                return Err(std::io::Error::other(
                    "actor lifecycle event precedes durable creation marker",
                ));
            }
        }
    }
    Ok((records, expected, created))
}

fn apply_event(
    records: &mut BTreeMap<ActorRef, DurableActorRecord>,
    event: EventKind,
) -> std::io::Result<()> {
    match event {
        EventKind::Created => {
            return Err(std::io::Error::other(
                "actor lifecycle creation marker reached record replay",
            ));
        }
        EventKind::Admitted { admission } => {
            let admission = *admission;
            let actor = admission.actor;
            if records
                .insert(
                    actor,
                    DurableActorRecord {
                        admission,
                        application: None,
                        terminal: None,
                    },
                )
                .is_some()
            {
                return Err(std::io::Error::other(format!(
                    "duplicate durable admission for actor {actor}"
                )));
            }
        }
        EventKind::ApplicationPrepared {
            actor,
            binding_path,
            accepted_source,
        } => {
            let record = records.get_mut(&actor).ok_or_else(|| {
                std::io::Error::other(format!(
                    "application row precedes admission for actor {actor}"
                ))
            })?;
            if record
                .application
                .replace(DurableActorApplication {
                    binding_path,
                    conversation: None,
                    accepted_source,
                })
                .is_some()
            {
                return Err(std::io::Error::other(format!(
                    "duplicate application row for actor {actor}"
                )));
            }
        }
        EventKind::ApplicationBound {
            actor,
            conversation,
        } => {
            let application = records
                .get_mut(&actor)
                .and_then(|record| record.application.as_mut())
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "binding row precedes application preparation for actor {actor}"
                    ))
                })?;
            if application.conversation.replace(conversation).is_some() {
                return Err(std::io::Error::other(format!(
                    "duplicate binding row for actor {actor}"
                )));
            }
        }
        EventKind::Retired { actor, terminal } => {
            let record = records.get_mut(&actor).ok_or_else(|| {
                std::io::Error::other(format!("terminal row precedes admission for actor {actor}"))
            })?;
            if record.terminal.replace(terminal).is_some() {
                return Err(std::io::Error::other(format!(
                    "duplicate terminal row for actor {actor}"
                )));
            }
        }
    }
    Ok(())
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
    use crate::{ActorId, ActorPlacement, EffectiveRole, Incarnation};
    use tidepool_codegen::scope::ScopeId;
    use tidepool_codegen::suspension::RealmId;
    use tidepool_repr::SessionId;

    fn descriptor(label: &str) -> ActorDescriptor {
        ActorDescriptor::new(
            label,
            ActorPlacement {
                session: SessionId(1),
                lexical_scope: ScopeId(1),
                resource_scope: RealmId(1),
            },
        )
        .with_effective_role(EffectiveRole::coding())
    }

    #[test]
    fn lifecycle_reopens_with_active_and_terminal_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("actors.jsonl");
        let actor = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(3),
        };
        let journal = ActorRecoveryJournal::open(&path).unwrap();
        journal
            .admit(actor, &descriptor("worker"), &["tree-1".into()])
            .unwrap();
        drop(journal);
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.is_empty(), "journal append produced no bytes");
        for row in stored.lines() {
            parse_row(row).unwrap();
        }
        let journal = ActorRecoveryJournal::open(&path).unwrap();
        let records = journal.records();
        assert_eq!(records.len(), 1, "stored rows: {stored}");
        assert_eq!(records[0].admission.actor, actor);
        assert_eq!(records[0].admission.launch_worktrees, ["tree-1"]);
        assert!(records[0].application.is_none());
        assert!(records[0].terminal.is_none());
        journal
            .prepare_application(
                actor,
                directory.path().join("binding.json"),
                Some("source-a".into()),
            )
            .unwrap();
        journal
            .bind_application(actor, "conversation-7".into())
            .unwrap();
        journal
            .retire(actor, ActorExitKind::Completed, "done".into())
            .unwrap();
        drop(journal);
        let records = ActorRecoveryJournal::open(path).unwrap().records();
        assert_eq!(
            records[0]
                .application
                .as_ref()
                .unwrap()
                .conversation
                .as_deref(),
            Some("conversation-7")
        );
        assert_eq!(records[0].terminal.as_ref().unwrap().summary, "done");
    }

    #[test]
    fn observed_read_leaves_a_live_journal_and_its_torn_tail_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("actors.jsonl");
        let journal = ActorRecoveryJournal::open(&path).unwrap();
        journal
            .admit(ActorRef::first(ActorId(4)), &descriptor("observed"), &[])
            .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"version\":2,\"sequence\":3,");
        std::fs::write(&path, &bytes).unwrap();
        let records = ActorRecoveryJournal::read_observed(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].admission.label, "observed");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let missing = directory.path().join("missing.jsonl");
        assert!(ActorRecoveryJournal::read_observed(&missing)
            .unwrap()
            .is_empty());
        assert!(!missing.exists());
    }

    #[test]
    fn identity_reuse_with_changed_parameters_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let journal = ActorRecoveryJournal::open(directory.path().join("actors.jsonl")).unwrap();
        let actor = ActorRef::first(ActorId(2));
        journal.admit(actor, &descriptor("first"), &[]).unwrap();
        let error = journal
            .admit(actor, &descriptor("changed"), &[])
            .unwrap_err();
        assert!(error.to_string().contains("reused"));
    }

    #[test]
    fn every_application_publication_boundary_reopens_without_inventing_progress() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("actors.jsonl");
        let binding = directory.path().join("binding.json");
        let actor = ActorRef::first(ActorId(9));

        let journal = ActorRecoveryJournal::open(&path).unwrap();
        journal.admit(actor, &descriptor("worker"), &[]).unwrap();
        drop(journal);
        let journal = ActorRecoveryJournal::open(&path).unwrap();
        assert!(journal.records()[0].application.is_none());

        journal
            .prepare_application(actor, binding.clone(), Some("source-revision".into()))
            .unwrap();
        drop(journal);
        let journal = ActorRecoveryJournal::open(&path).unwrap();
        let prepared = journal
            .records()
            .into_iter()
            .next()
            .unwrap()
            .application
            .unwrap();
        assert_eq!(prepared.binding_path, binding);
        assert_eq!(prepared.accepted_source.as_deref(), Some("source-revision"));
        assert!(prepared.conversation.is_none());

        journal
            .bind_application(actor, "conversation-9".into())
            .unwrap();
        drop(journal);
        let journal = ActorRecoveryJournal::open(&path).unwrap();
        assert_eq!(
            journal.records()[0]
                .application
                .as_ref()
                .unwrap()
                .conversation
                .as_deref(),
            Some("conversation-9")
        );
    }

    #[test]
    fn recovered_host_requires_the_original_lifecycle_owner() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.jsonl");
        assert_eq!(
            ActorRecoveryJournal::open_existing(&missing)
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::NotFound
        );

        let empty = directory.path().join("empty.jsonl");
        std::fs::write(&empty, b"").unwrap();
        assert!(ActorRecoveryJournal::open_existing(&empty)
            .err()
            .unwrap()
            .to_string()
            .contains("creation marker"));

        let initialized = directory.path().join("initialized.jsonl");
        drop(ActorRecoveryJournal::open(&initialized).unwrap());
        ActorRecoveryJournal::open_existing(&initialized).unwrap();
    }
}
