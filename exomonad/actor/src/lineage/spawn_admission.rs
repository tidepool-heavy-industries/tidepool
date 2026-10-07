//! Per-child admission retained under the checkpoint owner's transaction lock.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnAdmissionOutcome {
    Pending,
    Ready(ActorRef),
    Failed { child: Option<ActorRef>, detail: String },
}

pub(super) struct SpawnAdmissionRecord {
    owner: ActorRef,
    child: Option<ActorRef>,
    outcome: tokio::sync::watch::Sender<SpawnAdmissionOutcome>,
}

/// Exact run-issued admission authority. Clones observe the same retained
/// decision and cannot issue a second child or change a settled outcome.
#[derive(Clone)]
pub struct SpawnAdmission {
    id: uuid::Uuid,
    registry: ForkGroupRegistry,
}

pub(crate) struct ClaimedSpawn {
    pub path: ActorPath,
    pub authority: SpawnAdmission,
    pub checkpoint: Option<(CheckpointLease, Option<HostedCheckpointAttachment>)>,
}

impl ForkGroupRegistry {
    pub(crate) fn claim_spawn(
        &self,
        owner: ActorRef,
        session: SessionId,
        checkpoint: Option<&str>,
    ) -> Result<ClaimedSpawn, CheckpointRefusal> {
        let mut state = self.state.lock();
        if state.cleaning.contains(&owner) {
            return Err(CheckpointRefusal::UnavailableCheckpoint);
        }
        // Release and admission linearize on this same lock. The admitted
        // share remains usable even if the public checkpoint is released.
        let checkpoint = checkpoint
            .map(|token| self.checkpoint_admission_locked(&state, token, session))
            .transpose()?;
        let id = uuid::Uuid::new_v4();
        let path = ActorPath::parse(&format!("spawn-{id}"))
            .expect("runtime generated actor path is valid");
        self.lineage.retain_external(path.clone());
        state.spawns.insert(id, SpawnAdmissionRecord {
            owner,
            child: None,
            outcome: tokio::sync::watch::channel(SpawnAdmissionOutcome::Pending).0,
        });
        Ok(ClaimedSpawn {
            path,
            authority: SpawnAdmission { id, registry: self.clone() },
            checkpoint,
        })
    }
}

impl SpawnAdmission {
    pub(crate) fn bind(&self, owner: ActorRef, child: ActorRef) -> Result<(), String> {
        let mut state = self.registry.state.lock();
        let record = state.spawns.get_mut(&self.id).ok_or("spawn admission unavailable")?;
        if record.owner != owner || record.child.is_some_and(|bound| bound != child) {
            return Err("spawn admission incarnation mismatch".into());
        }
        if !matches!(*record.outcome.borrow(), SpawnAdmissionOutcome::Pending) {
            return Err("spawn admission already settled".into());
        }
        record.child = Some(child);
        Ok(())
    }

    /// Called after the provider attachment owner acknowledged the exact
    /// installed actor. Startup, workspace, context and tools are ready first.
    pub fn acknowledge(&self, child: ActorRef) -> Result<(), String> {
        let mut state = self.registry.state.lock();
        let record = state.spawns.get_mut(&self.id).ok_or("spawn admission unavailable")?;
        if record.child != Some(child) {
            return Err("spawn acknowledgement names another incarnation".into());
        }
        let current = record.outcome.borrow().clone();
        match current {
            SpawnAdmissionOutcome::Pending => {
                record.outcome.send_replace(SpawnAdmissionOutcome::Ready(child));
                Ok(())
            }
            SpawnAdmissionOutcome::Ready(actual) if actual == child => Ok(()),
            _ => Err("spawn admission already failed".into()),
        }
    }

    pub fn fail(&self, detail: String) {
        let mut state = self.registry.state.lock();
        if let Some(record) = state.spawns.get_mut(&self.id) {
            if matches!(*record.outcome.borrow(), SpawnAdmissionOutcome::Pending) {
                record.outcome.send_replace(SpawnAdmissionOutcome::Failed {
                    child: record.child, detail,
                });
            }
        }
    }

    pub async fn wait_ready(&self) -> Result<ActorRef, String> {
        let mut outcome = {
            let state = self.registry.state.lock();
            state.spawns.get(&self.id).ok_or("spawn admission unavailable")?.outcome.subscribe()
        };
        loop {
            let decision = outcome.borrow_and_update().clone();
            match decision {
                SpawnAdmissionOutcome::Ready(child) => return Ok(child),
                SpawnAdmissionOutcome::Failed { detail, .. } => return Err(detail),
                SpawnAdmissionOutcome::Pending => {},
            }
            outcome.changed().await.map_err(|_| "spawn admission owner stopped".to_owned())?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn independent_admissions_keep_exact_terminal_replay_and_ignore_labels() {
        let registry = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(crate::ActorId(1));
        let first = registry.claim_spawn(owner, SessionId(1), None).unwrap();
        let second = registry.claim_spawn(owner, SessionId(1), None).unwrap();
        assert_ne!(first.path, second.path);
        let child = ActorRef::first(crate::ActorId(2));
        first.authority.bind(owner, child).unwrap();
        assert!(first.authority.acknowledge(owner).is_err());
        second.authority.fail("workspace refused".into());
        first.authority.acknowledge(child).unwrap();
        first.authority.acknowledge(child).unwrap();
        first.authority.fail("late transport error".into());
        assert_eq!(first.authority.wait_ready().await.unwrap(), child);
        assert_eq!(first.authority.wait_ready().await.unwrap(), child);
        assert_eq!(second.authority.wait_ready().await.unwrap_err(), "workspace refused");
        assert!(first.authority.bind(owner, ActorRef::first(crate::ActorId(3))).is_err());
    }
}
