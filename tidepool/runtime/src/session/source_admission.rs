//! Source admission closes independently of lexical/native custody.
use super::NotebookGenerationId;
use parking_lot::Mutex;
use std::sync::Arc;
use tidepool_codegen::scope::ScopeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAdmissionPhase {
    Open,
    Fenced { attempt: u64 },
    Retired { successor: NotebookGenerationId },
    ScopeRetired,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceAdmissionError {
    #[error("source admission for scope {scope:?} is {phase:?}")]
    Closed {
        scope: ScopeId,
        phase: SourceAdmissionPhase,
    },
    #[error("source-dependent admission has no issuing world")]
    Unqualified,
    #[error("initial source publication is unavailable")]
    BootstrapUnavailable,
    #[error(
        "initial source publication retains {claims} active and {unconfirmed} unconfirmed actions"
    )]
    BootstrapBusy { claims: usize, unconfirmed: usize },
    #[error("source retirement attempt identity is exhausted")]
    Exhausted,
    #[error("source retirement attempt no longer owns its fence")]
    StaleFence,
    #[error("source admission for scope {scope:?} retains {count} unconfirmed actions")]
    Unconfirmed { scope: ScopeId, count: usize },
}

/// An additional issuer-owned admission check for the current compiler action.
/// The issuer retains any earned custody through its actual native END owner.
pub type SourceAdmissionHook = Arc<dyn Fn() -> Result<(), SourceAdmissionError> + Send + Sync>;

struct AdmissionState {
    next_attempt: u64,
    phase: SourceAdmissionPhase,
    claims: usize,
    unconfirmed: usize,
}
struct SourceAdmissionOwner {
    scope: ScopeId,
    prerequisite: Option<RuntimeSourceAdmission>,
    state: Mutex<AdmissionState>,
    settled: tokio::sync::Notify,
}

/// Issued by the canonical lexical-scope owner. A clone cannot reopen it.
#[derive(Clone)]
pub struct RuntimeSourceAdmission(Arc<SourceAdmissionOwner>);

impl RuntimeSourceAdmission {
    pub(super) fn new(scope: ScopeId, prerequisite: Option<Self>) -> Self {
        Self(Arc::new(SourceAdmissionOwner {
            scope,
            prerequisite,
            state: Mutex::new(AdmissionState {
                next_attempt: 0,
                phase: SourceAdmissionPhase::Open,
                claims: 0,
                unconfirmed: 0,
            }),
            settled: tokio::sync::Notify::new(),
        }))
    }
    pub fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn depends_on(&self, other: &Self) -> bool {
        self.same_owner(other)
            || self
                .0
                .prerequisite
                .as_ref()
                .is_some_and(|parent| parent.depends_on(other))
    }
    pub fn prerequisite(&self) -> Option<&Self> {
        self.0.prerequisite.as_ref()
    }

    /// Only synchronous registration/enqueue belongs in this callback. Locks
    /// remain held in ancestor order through the owner's actual reservation.
    pub fn with_open_admission<T>(
        &self,
        admit: impl FnOnce() -> T,
    ) -> Result<T, SourceAdmissionError> {
        let mut guards = Vec::new();
        self.lock_open_chain(&mut guards)?;
        Ok(admit())
    }

    fn lock_open_chain<'a>(
        &'a self,
        guards: &mut Vec<parking_lot::MutexGuard<'a, AdmissionState>>,
    ) -> Result<(), SourceAdmissionError> {
        if let Some(parent) = &self.0.prerequisite {
            parent.lock_open_chain(guards)?;
        }
        let state = self.0.state.lock();
        if state.unconfirmed != 0 {
            return Err(SourceAdmissionError::Unconfirmed {
                scope: self.0.scope,
                count: state.unconfirmed,
            });
        }
        if state.phase != SourceAdmissionPhase::Open {
            return Err(SourceAdmissionError::Closed {
                scope: self.0.scope,
                phase: state.phase,
            });
        }
        guards.push(state);
        Ok(())
    }

    /// Records one actual source action before allocation. A retained claim
    /// permits its admitted action to finish; every new action earns a claim.
    pub fn admit(&self) -> Result<RuntimeSourceClaim, SourceAdmissionError> {
        let prerequisite = self.0.prerequisite.as_ref().map(Self::admit).transpose()?;
        let mut state = self.0.state.lock();
        if state.unconfirmed != 0 {
            return Err(SourceAdmissionError::Unconfirmed {
                scope: self.0.scope,
                count: state.unconfirmed,
            });
        }
        match state.phase {
            SourceAdmissionPhase::Open => {
                state.claims += 1;
                Ok(RuntimeSourceClaim {
                    owner: self.clone(),
                    prerequisite: prerequisite.map(Box::new),
                    release_on_drop: true,
                })
            }
            phase => Err(SourceAdmissionError::Closed {
                scope: self.0.scope,
                phase,
            }),
        }
    }
    pub fn fence(&self) -> Result<RuntimeSourceFence, SourceAdmissionError> {
        let mut state = self.0.state.lock();
        if state.phase != SourceAdmissionPhase::Open {
            return Err(SourceAdmissionError::Closed {
                scope: self.0.scope,
                phase: state.phase,
            });
        }
        let attempt = state.next_attempt;
        state.next_attempt = attempt
            .checked_add(1)
            .ok_or(SourceAdmissionError::Exhausted)?;
        state.phase = SourceAdmissionPhase::Fenced { attempt };
        Ok(RuntimeSourceFence {
            owner: self.clone(),
            attempt,
        })
    }

    pub(super) fn retire_scope(&self, scope: ScopeId) {
        if self.0.scope == scope {
            self.0.state.lock().phase = SourceAdmissionPhase::ScopeRetired;
            self.0.settled.notify_waiters();
        }
    }
}
impl std::fmt::Debug for RuntimeSourceAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSourceAdmission")
            .field("scope", &self.0.scope)
            .finish_non_exhaustive()
    }
}
impl PartialEq for RuntimeSourceAdmission {
    fn eq(&self, other: &Self) -> bool {
        self.same_owner(other)
    }
}
impl Eq for RuntimeSourceAdmission {}

#[must_use]
pub struct RuntimeSourceClaim {
    owner: RuntimeSourceAdmission,
    prerequisite: Option<Box<RuntimeSourceClaim>>,
    release_on_drop: bool,
}
impl RuntimeSourceClaim {
    /// The native owner could not prove END. Keep the admitted obligation in
    /// the same gate; losing the waiter is not confirmed resource release.
    pub fn retain_unconfirmed(mut self) {
        self.release_on_drop = false;
        {
            let mut state = self.owner.0.state.lock();
            state.claims -= 1;
            state.unconfirmed += 1;
        }
        self.owner.0.settled.notify_waiters();
        if let Some(prerequisite) = self.prerequisite.take() {
            prerequisite.retain_unconfirmed();
        }
    }
}
impl Drop for RuntimeSourceClaim {
    fn drop(&mut self) {
        if self.release_on_drop {
            self.owner.0.state.lock().claims -= 1;
            self.owner.0.settled.notify_waiters();
        }
    }
}

/// Affine retirement attempt. Abandonment keeps admission fenced.
#[must_use]
pub struct RuntimeSourceFence {
    owner: RuntimeSourceAdmission,
    attempt: u64,
}
impl RuntimeSourceFence {
    pub fn pending_claims(&self) -> usize {
        let state = self.owner.0.state.lock();
        state.claims + state.unconfirmed
    }
    pub async fn wait_for_claims(&self) -> Result<(), SourceAdmissionError> {
        loop {
            let settled = self.owner.0.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            {
                let state = self.owner.0.state.lock();
                if state.unconfirmed != 0 {
                    return Err(SourceAdmissionError::Unconfirmed {
                        scope: self.owner.0.scope,
                        count: state.unconfirmed,
                    });
                }
                if state.claims == 0 {
                    return Ok(());
                }
            }
            settled.await;
        }
    }
    pub fn reopen(self) -> Result<(), SourceAdmissionError> {
        self.settle(SourceAdmissionPhase::Open)
    }
    pub fn retire(self, successor: NotebookGenerationId) -> Result<(), SourceAdmissionError> {
        self.settle(SourceAdmissionPhase::Retired { successor })
    }
    fn settle(self, phase: SourceAdmissionPhase) -> Result<(), SourceAdmissionError> {
        let mut state = self.owner.0.state.lock();
        if state.phase
            != (SourceAdmissionPhase::Fenced {
                attempt: self.attempt,
            })
        {
            return Err(SourceAdmissionError::StaleFence);
        }
        if state.unconfirmed != 0 {
            return Err(SourceAdmissionError::Unconfirmed {
                scope: self.owner.0.scope,
                count: state.unconfirmed,
            });
        }
        if matches!(phase, SourceAdmissionPhase::Retired { .. }) && state.claims != 0 {
            return Err(SourceAdmissionError::Closed {
                scope: self.owner.0.scope,
                phase: state.phase,
            });
        }
        state.phase = phase;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{PersistentSession, SessionError};
    use proptest::prelude::*;

    #[test]
    fn fencing_source_refuses_late_binding_without_retiring_custody() {
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let scope = session.mint_isolated_scope_with_lease().unwrap();
        let mut before =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "before", 70);
        before.scope = scope.scope();
        session
            .bind_replacing_decls_in(scope.scope(), vec![before], &mut |_| {})
            .unwrap();
        let mut late =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "late", 71);
        late.scope = scope.scope();
        let before_handles = session.value_handle_count();
        let fence = scope.source_admission().fence().unwrap();
        assert!(matches!(
            session.bind_replacing_decls_in(scope.scope(), vec![late], &mut |_| {}),
            Err(SessionError::SourceAdmission(
                SourceAdmissionError::Closed { .. }
            ))
        ));
        assert!(session.resolve_in(scope.scope(), "before").is_some());
        assert!(session.resolve_in(scope.scope(), "late").is_none());
        assert_eq!(session.value_handle_count() + 1, before_handles);
        assert!(session.scope_tree().is_live(scope.scope()));
        fence.reopen().unwrap();
        scope.source_admission().admit().unwrap();
    }

    #[tokio::test]
    async fn admitted_claim_outlives_fence_and_unconfirmed_end_keeps_it_closed() {
        let mut session = PersistentSession::new(None, 1024);
        let scope = session.mint_isolated_scope_with_lease().unwrap();
        let claim = scope.source_admission().admit().unwrap();
        let fence = scope.source_admission().fence().unwrap();
        assert_eq!(fence.pending_claims(), 1);
        assert!(scope.source_admission().admit().is_err());
        claim.retain_unconfirmed();
        assert!(matches!(
            fence.wait_for_claims().await,
            Err(SourceAdmissionError::Unconfirmed { count: 1, .. })
        ));
        assert!(matches!(
            fence.reopen(),
            Err(SourceAdmissionError::Unconfirmed { .. })
        ));
        assert!(scope.source_admission().with_open_admission(|| ()).is_err());
    }

    #[test]
    fn captured_scopes_keep_dependency_but_independent_notebook_issues_its_own() {
        let mut session = PersistentSession::new(None, 1024);
        let parent = session.mint_isolated_scope_with_lease().unwrap();
        let retained = session.retain_lexical_scope(parent.scope()).unwrap();
        let inherited = session.mint_scope_from_lease(&retained).unwrap();
        let child = session
            .adopt_initial_notebook_scope_with_source(
                inherited,
                Some(parent.source_admission().clone()),
            )
            .unwrap();
        let independent = session.mint_notebook_scope_with_lease(None).unwrap();
        assert!(child
            .source_admission()
            .depends_on(parent.source_admission()));
        let fence = parent.source_admission().fence().unwrap();
        assert!(retained.source_admission().admit().is_err());
        assert!(child.source_admission().admit().is_err());
        independent.source_admission().admit().unwrap();
        drop(fence);
        assert!(
            parent.source_admission().admit().is_err(),
            "abandoned fence cannot reopen"
        );
    }

    proptest! {
        #[test]
        fn generated_admission_histories_match_owner_model(history in prop::collection::vec((0_u8..7, 0_usize..2, 0_usize..8), 1..96)) {
            let mut session = PersistentSession::new(None, 1024);
            let parent = session.mint_isolated_scope_with_lease().unwrap();
            let child = session.mint_notebook_scope_with_lease(Some(parent.source_admission().clone())).unwrap();
            let gates = [parent.source_admission(), child.source_admission()];
            let mut phases = [0_u8; 2]; // independent model: open/fenced/retired
            let mut fences: [Option<RuntimeSourceFence>; 2] = [None, None];
            let mut claims: Vec<Option<(usize, RuntimeSourceClaim)>> = (0..8).map(|_| None).collect();
            let mut uncertainty = [0_usize; 2];
            let mut admitted = 0;
            let mut refused = 0;
            // This prefix reaches inherited refusal and confirmed reopening in
            // every case, independently of the generated suffix distribution.
            let prefix = [(0, 1, 0), (1, 0, 0), (0, 1, 1), (4, 0, 0), (2, 0, 0)];
            for (operation, gate, slot) in prefix.into_iter().chain(history) {
                let open = phases[gate] == 0 && uncertainty[gate] == 0 && (gate == 0 || (phases[0] == 0 && uncertainty[0] == 0));
                match operation {
                    0 if claims[slot].is_none() => {
                        let result = gates[gate].admit();
                        prop_assert_eq!(result.is_ok(), open);
                        if let Ok(claim) = result { claims[slot] = Some((gate, claim)); admitted += 1; }
                        else { refused += 1; }
                    }
                    1 => {
                        let result = gates[gate].fence();
                        prop_assert_eq!(result.is_ok(), phases[gate] == 0);
                        if let Ok(fence) = result { phases[gate] = 1; fences[gate] = Some(fence); }
                    }
                    2 => if let Some(fence) = fences[gate].take() {
                        let result = fence.reopen();
                        prop_assert_eq!(result.is_ok(), uncertainty[gate] == 0);
                        if result.is_ok() { phases[gate] = 0; }
                    },
                    3 => if let Some(fence) = fences[gate].take() {
                        let outstanding = claims.iter().flatten().filter(|(owner, _)| gate == 0 || *owner == gate).count() + uncertainty[gate];
                        let result = fence.retire(NotebookGenerationId::INITIAL.next().unwrap());
                        prop_assert_eq!(result.is_ok(), outstanding == 0);
                        if result.is_ok() { phases[gate] = 2; }
                    },
                    4 => { claims[slot] = None; },
                    5 => if let Some((owner, claim)) = claims[slot].take() {
                        uncertainty[owner] += 1;
                        if owner == 1 { uncertainty[0] += 1; }
                        claim.retain_unconfirmed();
                    },
                    6 => { fences[gate] = None; },
                    _ => {},
                }
                for checked in 0..2 {
                    let expected = phases[checked] == 0 && uncertainty[checked] == 0 && (checked == 0 || (phases[0] == 0 && uncertainty[0] == 0));
                    let mut entered = false;
                    let result = gates[checked].with_open_admission(|| entered = true);
                    prop_assert_eq!(entered, expected);
                    prop_assert_eq!(result.is_ok(), expected);
                }
            }
            prop_assert!(admitted > 0 && refused > 0);
        }
    }
}
