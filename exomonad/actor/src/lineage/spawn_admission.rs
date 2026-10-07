//! Per-child admission retained under the checkpoint owner's transaction lock.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnAdmissionOutcome {
    Pending,
    Ready(ActorRef),
    Failed {
        child: Option<ActorRef>,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnCleanupOutcome {
    NotNeeded,
    Confirmed,
    Unconfirmed(String),
}

pub(super) struct SpawnAdmissionRecord {
    owner: ActorRef,
    child: Option<ActorRef>,
    workspace: Option<tidepool_bridge_effects::WtWorktreeHandle>,
    cleanup: SpawnCleanupOutcome,
    sponsors: Vec<ActorRef>,
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum SpawnClaimError {
    #[error("checkpoint refusal: {0:?}")]
    Checkpoint(CheckpointRefusal),
    #[error("actor {0:?} admission is closed")]
    Closing(ActorRef),
    #[error("actor {sponsor:?} descendant limit {maximum} is exhausted")]
    DescendantLimit { sponsor: ActorRef, maximum: usize },
}

impl ForkGroupRegistry {
    pub(crate) fn claim_spawn(
        &self,
        owner: ActorRef,
        session: SessionId,
        checkpoint: Option<&str>,
        maximum_descendants: Option<usize>,
    ) -> Result<ClaimedSpawn, SpawnClaimError> {
        let mut state = self.state.lock();
        if state.cleaning.contains(&owner) {
            return Err(SpawnClaimError::Closing(owner));
        }
        // Release and admission linearize on this same lock. The admitted
        // share remains usable even if the public checkpoint is released.
        let checkpoint = checkpoint
            .map(|token| self.checkpoint_admission_locked(&state, token, session))
            .transpose()
            .map_err(SpawnClaimError::Checkpoint)?;
        if let Some(maximum) = maximum_descendants {
            state
                .descendant_limits
                .entry(owner)
                .and_modify(|old| *old = (*old).min(maximum))
                .or_insert(maximum);
        }
        let mut sponsors = checkpoint
            .as_ref()
            .map(|(lease, _)| lease.budget_sponsors.clone())
            .unwrap_or_default();
        let mut ancestor = Some(owner);
        while let Some(actor) = ancestor {
            if !sponsors.contains(&actor) {
                sponsors.push(actor);
            }
            ancestor = state.parents.get(&actor).copied();
        }
        for sponsor in &sponsors {
            if state.cleaning.contains(sponsor) {
                return Err(SpawnClaimError::Closing(*sponsor));
            }
            if let Some(maximum) = state.descendant_limits.get(sponsor).copied() {
                let charged = state
                    .spawns
                    .values()
                    .filter(|record| {
                        record.sponsors.contains(sponsor)
                            && match record.child {
                                Some(child) => {
                                    state.active.contains(&child)
                                        || matches!(
                                            *record.outcome.borrow(),
                                            SpawnAdmissionOutcome::Pending
                                        )
                                }
                                None => matches!(
                                    *record.outcome.borrow(),
                                    SpawnAdmissionOutcome::Pending
                                ),
                            }
                    })
                    .count();
                if charged >= maximum {
                    return Err(SpawnClaimError::DescendantLimit {
                        sponsor: *sponsor,
                        maximum,
                    });
                }
            }
        }
        let id = uuid::Uuid::new_v4();
        let path = ActorPath::parse(&format!("spawn-{id}"))
            .expect("runtime generated actor path is valid");
        self.lineage.retain_external(path.clone());
        state.spawns.insert(
            id,
            SpawnAdmissionRecord {
                owner,
                child: None,
                workspace: None,
                cleanup: SpawnCleanupOutcome::NotNeeded,
                sponsors,
                outcome: tokio::sync::watch::channel(SpawnAdmissionOutcome::Pending).0,
            },
        );
        Ok(ClaimedSpawn {
            path,
            authority: SpawnAdmission {
                id,
                registry: self.clone(),
            },
            checkpoint,
        })
    }
}

impl SpawnAdmission {
    pub(crate) fn reserve_child(&self, owner: ActorRef, child: ActorRef) -> Result<(), String> {
        let mut state = self.registry.state.lock();
        let record = state
            .spawns
            .get_mut(&self.id)
            .ok_or("spawn admission unavailable")?;
        if record.owner != owner || record.child.is_some_and(|bound| bound != child) {
            return Err("spawn admission incarnation mismatch".into());
        }
        if !matches!(*record.outcome.borrow(), SpawnAdmissionOutcome::Pending) {
            return Err("spawn admission already settled".into());
        }
        record.child = Some(child);
        Ok(())
    }

    pub(crate) fn bind(&self, owner: ActorRef, child: ActorRef) -> Result<(), String> {
        let mut state = self.registry.state.lock();
        let record = state
            .spawns
            .get_mut(&self.id)
            .ok_or("spawn admission unavailable")?;
        if record.owner != owner || record.child.is_some_and(|bound| bound != child) {
            return Err("spawn admission incarnation mismatch".into());
        }
        if !matches!(*record.outcome.borrow(), SpawnAdmissionOutcome::Pending) {
            return Err("spawn admission already settled".into());
        }
        record.child = Some(child);
        state.parents.insert(child, owner);
        state.active.insert(child);
        Ok(())
    }

    pub(crate) fn retain_workspace(&self, workspace: tidepool_bridge_effects::WtWorktreeHandle) {
        if let Some(record) = self.registry.state.lock().spawns.get_mut(&self.id) {
            record.workspace = Some(workspace);
        }
    }

    pub(crate) fn retain_cleanup(&self, cleanup: SpawnCleanupOutcome) {
        if let Some(record) = self.registry.state.lock().spawns.get_mut(&self.id) {
            record.cleanup = cleanup;
        }
    }

    pub(crate) fn error(&self, detail: String) -> crate::start::SpawnError {
        let state = self.registry.state.lock();
        let Some(record) = state.spawns.get(&self.id) else {
            return crate::start::SpawnError::SpawnRefused(detail);
        };
        match record.child {
            Some(child) => crate::start::SpawnError::SpawnPartiallyStarted(
                (child.id.0 as i64, child.incarnation.0 as i64),
                record.workspace.clone(),
                match &record.cleanup {
                    SpawnCleanupOutcome::Unconfirmed(cleanup) => {
                        format!("{detail}; cleanup unconfirmed: {cleanup}")
                    }
                    SpawnCleanupOutcome::Confirmed => format!("{detail}; actor cleanup confirmed"),
                    SpawnCleanupOutcome::NotNeeded => detail,
                },
            ),
            None => crate::start::SpawnError::SpawnRefused(detail),
        }
    }

    /// Called after the provider attachment owner acknowledged the exact
    /// installed actor. Startup, workspace, context and tools are ready first.
    pub fn acknowledge(&self, child: ActorRef) -> Result<(), String> {
        let mut state = self.registry.state.lock();
        let record = state
            .spawns
            .get_mut(&self.id)
            .ok_or("spawn admission unavailable")?;
        if record.child != Some(child) {
            return Err("spawn acknowledgement names another incarnation".into());
        }
        let current = record.outcome.borrow().clone();
        match current {
            SpawnAdmissionOutcome::Pending => {
                record
                    .outcome
                    .send_replace(SpawnAdmissionOutcome::Ready(child));
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
                    child: record.child,
                    detail,
                });
            }
        }
    }

    pub fn outcome(&self) -> SpawnAdmissionOutcome {
        self.registry
            .state
            .lock()
            .spawns
            .get(&self.id)
            .map(|record| record.outcome.borrow().clone())
            .unwrap_or_else(|| SpawnAdmissionOutcome::Failed {
                child: None,
                detail: "spawn admission unavailable".into(),
            })
    }

    pub async fn wait_ready(&self) -> Result<ActorRef, String> {
        let mut outcome = {
            let state = self.registry.state.lock();
            state
                .spawns
                .get(&self.id)
                .ok_or("spawn admission unavailable")?
                .outcome
                .subscribe()
        };
        loop {
            let decision = outcome.borrow_and_update().clone();
            match decision {
                SpawnAdmissionOutcome::Ready(child) => return Ok(child),
                SpawnAdmissionOutcome::Failed { detail, .. } => return Err(detail),
                SpawnAdmissionOutcome::Pending => {}
            }
            outcome
                .changed()
                .await
                .map_err(|_| "spawn admission owner stopped".to_owned())?;
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
        let first = registry
            .claim_spawn(owner, SessionId(1), None, None)
            .unwrap();
        let second = registry
            .claim_spawn(owner, SessionId(1), None, None)
            .unwrap();
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
        assert_eq!(
            second.authority.wait_ready().await.unwrap_err(),
            "workspace refused"
        );
        assert!(first
            .authority
            .bind(owner, ActorRef::first(crate::ActorId(3)))
            .is_err());
    }
    fn checkpoint(registry: &ForkGroupRegistry, issuer: ActorRef) -> String {
        registry.capture_checkpoint(
            "exact cut".into(),
            issuer,
            crate::ActorCapabilities::default().with_descendant_budget(crate::DescendantBudget {
                maximum_depth: 4,
                maximum_active_children: Some(1),
            }),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(1),
            ScopeId(1),
            WorkbenchForkBoundary::Route {
                actor_id: issuer.id.0,
                incarnation: issuer.incarnation.0,
                watch_id: 1,
            },
        )
    }

    #[test]
    fn checkpoint_spawn_claim_linearizes_release_and_preserves_admitted_share() {
        for _ in 0..64 {
            let registry = ForkGroupRegistry::new(ActorLineageRegistry::default());
            let owner = ActorRef::first(crate::ActorId(1));
            let token = checkpoint(&registry, owner);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let releasing = registry.clone();
            let release_token = token.clone();
            let ready = barrier.clone();
            let task = std::thread::spawn(move || {
                ready.wait();
                releasing
                    .release_checkpoint(&release_token, SessionId(1))
                    .unwrap();
            });
            barrier.wait();
            let admitted = registry.claim_spawn(owner, SessionId(1), Some(&token), None);
            task.join().unwrap();
            match admitted {
                Ok(claim) => {
                    assert_eq!(claim.checkpoint.unwrap().0.scope, ScopeId(1));
                    claim
                        .authority
                        .fail("test releases unstarted reservation".into());
                }
                Err(SpawnClaimError::Checkpoint(CheckpointRefusal::ReleasedCheckpoint)) => {}
                other => panic!("unexpected release race outcome: {}", other.err().unwrap()),
            }
            assert!(matches!(
                registry.claim_spawn(owner, SessionId(1), Some(&token), None),
                Err(SpawnClaimError::Checkpoint(
                    CheckpointRefusal::ReleasedCheckpoint
                ))
            ));
        }
    }

    #[test]
    fn checkpoint_issuer_and_creator_ceilings_charge_pending_and_live_children() {
        let registry = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(crate::ActorId(1));
        let creator = ActorRef::first(crate::ActorId(2));
        let token = checkpoint(&registry, issuer);
        assert!(matches!(
            registry.claim_spawn(creator, SessionId(2), Some(&token), None),
            Err(SpawnClaimError::Checkpoint(CheckpointRefusal::WrongSession))
        ));
        let admitted = registry
            .claim_spawn(creator, SessionId(1), Some(&token), Some(1))
            .unwrap();
        assert!(
            matches!(registry.claim_spawn(creator, SessionId(1), None, Some(1)),
            Err(SpawnClaimError::DescendantLimit { sponsor, maximum: 1 }) if sponsor == creator)
        );
        let stranger = ActorRef::first(crate::ActorId(3));
        assert!(
            matches!(registry.claim_spawn(stranger, SessionId(1), Some(&token), None),
            Err(SpawnClaimError::DescendantLimit { sponsor, maximum: 1 }) if sponsor == issuer)
        );
        let child = ActorRef::first(crate::ActorId(4));
        admitted.authority.bind(creator, child).unwrap();
        admitted.authority.acknowledge(child).unwrap();
        registry.release_checkpoint(&token, SessionId(1)).unwrap();
        // Revoking the checkpoint cannot forgive the creator's live work.
        assert!(matches!(
            registry.claim_spawn(creator, SessionId(1), None, Some(1)),
            Err(SpawnClaimError::DescendantLimit { .. })
        ));
        registry.retire_actor(child);
        registry
            .claim_spawn(creator, SessionId(1), None, Some(1))
            .unwrap();
    }

    #[test]
    fn failed_unbound_child_releases_only_its_own_reserved_capacity() {
        let registry = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(crate::ActorId(1));
        let first = registry
            .claim_spawn(owner, SessionId(1), None, Some(2))
            .unwrap();
        let second = registry
            .claim_spawn(owner, SessionId(1), None, Some(2))
            .unwrap();
        assert!(registry
            .claim_spawn(owner, SessionId(1), None, Some(2))
            .is_err());
        first.authority.fail("workspace refused".into());
        let third = registry
            .claim_spawn(owner, SessionId(1), None, Some(2))
            .unwrap();
        assert_eq!(second.authority.outcome(), SpawnAdmissionOutcome::Pending);
        assert_eq!(third.authority.outcome(), SpawnAdmissionOutcome::Pending);
        assert!(registry
            .claim_spawn(owner, SessionId(1), None, Some(2))
            .is_err());
    }
    #[test]
    fn pre_start_identity_failure_retains_identity_and_releases_reserved_capacity() {
        let registry = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(crate::ActorId(1));
        let failed = registry
            .claim_spawn(owner, SessionId(1), None, Some(1))
            .unwrap();
        let child = ActorRef::first(crate::ActorId(2));
        failed.authority.reserve_child(owner, child).unwrap();
        assert!(registry
            .claim_spawn(owner, SessionId(1), None, Some(1))
            .is_err());
        failed.authority.fail("pre_start journal refused".into());
        assert_eq!(
            failed.authority.outcome(),
            SpawnAdmissionOutcome::Failed {
                child: Some(child),
                detail: "pre_start journal refused".into(),
            }
        );
        assert!(matches!(
            failed.authority.error("pre_start journal refused".into()),
            crate::start::SpawnError::SpawnPartiallyStarted((2, 1), None, _)
        ));
        registry
            .claim_spawn(owner, SessionId(1), None, Some(1))
            .unwrap();
        assert!(!registry.state.lock().active.contains(&child));
    }
}
