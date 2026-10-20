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
    #[error("source compiler work requires its owning runtime transaction")]
    MissingTransaction,
    #[error("source compiler transactions cannot borrow a foreign or nested transaction")]
    ForeignTransaction,
    #[error("source lexical lease belongs to a stale issuing epoch")]
    StaleSourceLease,
    #[error("source work has no live canonical target lease for scope {scope:?}")]
    MissingTargetLease { scope: ScopeId },
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
    #[error("source fence settlement did not commit its scoped reopen")]
    ReopenNotCommitted,
    #[error("source cutoff did not commit its scoped fence")]
    FenceNotCommitted,
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

    /// Gate state alone is not source authority; canonical lease consumers also
    /// hold the issuing epochs through their synchronous reservation.
    #[cfg(test)]
    pub(crate) fn with_open_admission<T>(
        &self,
        admit: impl FnOnce() -> T,
    ) -> Result<T, SourceAdmissionError> {
        Self::with_open_admissions([self], admit)
    }

    pub(super) fn with_open_admissions<'a, T>(
        admissions: impl IntoIterator<Item = &'a Self>,
        admit: impl FnOnce() -> T,
    ) -> Result<T, SourceAdmissionError> {
        fn collect(admission: &RuntimeSourceAdmission, gates: &mut Vec<RuntimeSourceAdmission>) {
            if gates.iter().any(|gate| gate.same_owner(admission)) {
                return;
            }
            if let Some(parent) = admission.prerequisite() {
                collect(parent, gates);
            }
            gates.push(admission.clone());
        }
        let mut gates = Vec::new();
        for admission in admissions {
            collect(admission, &mut gates);
        }
        // A total order also covers two unrelated source/target chains. A
        // recursive ancestor-only lock order would invert on overlapping DAGs.
        gates.sort_unstable_by_key(|gate| Arc::as_ptr(&gate.0) as usize);
        let mut guards = Vec::new();
        for gate in &gates {
            let state = gate.0.state.lock();
            if state.unconfirmed != 0 {
                return Err(SourceAdmissionError::Unconfirmed {
                    scope: gate.0.scope,
                    count: state.unconfirmed,
                });
            }
            if state.phase != SourceAdmissionPhase::Open {
                return Err(SourceAdmissionError::Closed {
                    scope: gate.0.scope,
                    phase: state.phase,
                });
            }
            guards.push(state);
        }
        Ok(admit())
    }

    /// Records one actual source action before allocation. A retained claim
    /// permits its admitted action to finish; every new action earns a claim.
    #[cfg(test)]
    pub(crate) fn admit(&self) -> Result<RuntimeSourceClaim, SourceAdmissionError> {
        let prerequisite = self.0.prerequisite.as_ref().map(Self::admit).transpose()?;
        let mut claim = self.admit_local()?;
        claim.prerequisite = prerequisite.map(Box::new);
        Ok(claim)
    }

    fn admit_local(&self) -> Result<RuntimeSourceClaim, SourceAdmissionError> {
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
                    prerequisite: None,
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
        self.fence_inner(false)
    }

    /// Compare the selected owner while canonical custody and this own gate
    /// remain locked. Commit must be the callback's last action. Active and
    /// uncertain claims remain visible to the retirement owner after cutoff.
    pub fn fence_with<E>(
        &self,
        lease: &Arc<super::RuntimeLexicalScopeLease>,
        compare: impl for<'a> FnOnce(RuntimeSourceFenceTransition<'a>) -> Result<(), E>,
    ) -> Result<Result<RuntimeSourceFence, SourceFenceTransitionFailure<E>>, SourceAdmissionError>
    {
        if !lease.source_admission().same_owner(self) {
            return Err(SourceAdmissionError::StaleSourceLease);
        }
        super::RuntimeLexicalScopeLease::with_source_leases([lease.clone()], |_| {
            let mut state = self.0.state.lock();
            if state.phase != SourceAdmissionPhase::Open {
                return Err(SourceAdmissionError::Closed {
                    scope: self.0.scope,
                    phase: state.phase,
                });
            }
            let attempt = state.next_attempt;
            let next_attempt = attempt
                .checked_add(1)
                .ok_or(SourceAdmissionError::Exhausted)?;
            let cutoff = ScopedSourceFence {
                state: &mut state,
                attempt,
                next_attempt,
            };
            let outcome = compare(RuntimeSourceFenceTransition {
                state: &mut *cutoff.state,
                attempt,
                next_attempt,
            });
            let committed = cutoff.state.phase == SourceAdmissionPhase::Fenced { attempt };
            match (outcome, committed) {
                (Ok(()), true) => Ok(Ok(RuntimeSourceFence {
                    owner: self.clone(),
                    attempt,
                })),
                (Ok(()), false) => Err(SourceAdmissionError::FenceNotCommitted),
                (Err(error), false) => Ok(Err(SourceFenceTransitionFailure::Refused(error))),
                (Err(error), true) => Ok(Err(SourceFenceTransitionFailure::Unconfirmed {
                    fence: RuntimeSourceFence {
                        owner: self.clone(),
                        attempt,
                    },
                    error,
                })),
            }
        })
    }
    /// Publication cannot expose its target while a source action is active.
    /// Busy refusal leaves the existing phase untouched under this same lock.
    pub fn fence_if_idle(&self) -> Result<RuntimeSourceFence, SourceAdmissionError> {
        self.fence_inner(true)
    }
    fn fence_inner(&self, require_idle: bool) -> Result<RuntimeSourceFence, SourceAdmissionError> {
        let mut state = self.0.state.lock();
        if state.phase != SourceAdmissionPhase::Open {
            return Err(SourceAdmissionError::Closed {
                scope: self.0.scope,
                phase: state.phase,
            });
        }
        if require_idle && (state.claims != 0 || state.unconfirmed != 0) {
            return Err(SourceAdmissionError::BootstrapBusy {
                claims: state.claims,
                unconfirmed: state.unconfirmed,
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
pub(crate) struct RuntimeSourceClaim {
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

/// One actual source action retains every exact ancestor even when scopes
/// share a gate. Issued only by a validated canonical lexical lease.
pub struct RuntimeSourceLeaseClaim {
    claims: Vec<RuntimeSourceClaim>,
    sources: Vec<Arc<super::RuntimeOriginalSource>>,
    leases: Vec<Arc<super::RuntimeLexicalScopeLease>>,
}
impl RuntimeSourceLeaseClaim {
    pub(super) fn admit(
        sources: &[Arc<super::RuntimeOriginalSource>],
        leases: &[Arc<super::RuntimeLexicalScopeLease>],
    ) -> Result<Self, SourceAdmissionError> {
        let mut gates: Vec<RuntimeSourceAdmission> = Vec::new();
        let mut claims = Vec::new();
        for source in sources {
            let gate = source.source_admission();
            if !gates.iter().any(|held| held.same_owner(gate)) {
                claims.push(gate.admit_local()?);
                gates.push(gate.clone());
            }
        }
        Ok(Self {
            claims,
            sources: sources.to_vec(),
            leases: leases.to_vec(),
        })
    }

    pub fn retain_unconfirmed(self) {
        for source in &self.sources {
            source.retain_unconfirmed_source_custody();
        }
        for lease in &self.leases {
            lease.retain_unconfirmed_source_custody();
        }
        for claim in self.claims {
            claim.retain_unconfirmed();
        }
    }
}

/// Affine retirement attempt. Abandonment keeps admission fenced.
#[must_use]
pub struct RuntimeSourceFence {
    owner: RuntimeSourceAdmission,
    attempt: u64,
}

/// Publication may observe its exact held fence without reopening or retiring it.
#[derive(Clone)]
pub struct RuntimeSourcePublicationView {
    owner: RuntimeSourceAdmission,
    attempt: u64,
}

impl RuntimeSourcePublicationView {
    pub(super) fn with_current<T>(
        &self,
        lease: &Arc<super::RuntimeLexicalScopeLease>,
        action: impl FnOnce() -> T,
    ) -> Result<T, SourceAdmissionError> {
        if !lease.source_admission().same_owner(&self.owner) {
            return Err(SourceAdmissionError::StaleFence);
        }
        super::RuntimeLexicalScopeLease::with_source_leases([lease.clone()], |_| {
            let state = self.owner.0.state.lock();
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
            if state.claims != 0 {
                return Err(SourceAdmissionError::BootstrapBusy {
                    claims: state.claims,
                    unconfirmed: 0,
                });
            }
            Ok(action())
        })
    }
}

/// A refusal before cutoff differs from a callback that failed after cutoff.
/// The latter returns the same newly issued fence with its primary error.
pub enum SourceFenceTransitionFailure<E> {
    Refused(E),
    Unconfirmed { fence: RuntimeSourceFence, error: E },
}

/// Borrowed transition of the existing gate. No independent phase flag can
/// claim a cutoff that the owning gate did not commit.
pub struct RuntimeSourceFenceTransition<'a> {
    state: &'a mut AdmissionState,
    attempt: u64,
    next_attempt: u64,
}

impl RuntimeSourceFenceTransition<'_> {
    pub fn commit(self) {
        self.state.next_attempt = self.next_attempt;
        self.state.phase = SourceAdmissionPhase::Fenced {
            attempt: self.attempt,
        };
    }
}

struct ScopedSourceFence<'a> {
    state: &'a mut AdmissionState,
    attempt: u64,
    next_attempt: u64,
}

impl Drop for ScopedSourceFence<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.state.next_attempt = self.next_attempt;
            self.state.phase = SourceAdmissionPhase::Fenced {
                attempt: self.attempt,
            };
        }
    }
}

/// Evidence that only this own gate reopened. Ancestor gates may still refuse
/// fresh work; this value grants no source admission.
#[derive(Debug)]
pub struct OwnSourceFenceReopened {
    _private: (),
}

/// A synchronous view of the existing locked fence. Its owner compares its
/// publication state before committing, without reacquiring a source gate.
pub struct RuntimeSourceReopen<'a> {
    state: &'a mut AdmissionState,
    committed: &'a mut bool,
}
impl RuntimeSourceReopen<'_> {
    pub fn commit(self) {
        self.state.phase = SourceAdmissionPhase::Open;
        *self.committed = true;
    }
}

struct ScopedReopen<'a> {
    state: &'a mut AdmissionState,
    original: SourceAdmissionPhase,
    committed: bool,
    accepted: bool,
}
impl Drop for ScopedReopen<'_> {
    fn drop(&mut self) {
        if !self.accepted {
            self.state.phase = self.original;
        }
    }
}
impl RuntimeSourceFence {
    pub fn publication_view(&self) -> RuntimeSourcePublicationView {
        RuntimeSourcePublicationView {
            owner: self.owner.clone(),
            attempt: self.attempt,
        }
    }

    pub fn unconfirmed_claims(&self) -> usize {
        self.owner.0.state.lock().unconfirmed
    }
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
    /// Test-only convenience for an unconditional reopen under the issuing
    /// lease. Production reopening must present its publication compare through
    /// [`Self::reopen_with`]; this wrapper exists so tests can settle without a
    /// compare while still running the canonical custody checks.
    #[cfg(test)]
    pub fn reopen(
        self,
        lease: &Arc<super::RuntimeLexicalScopeLease>,
    ) -> Result<(), SourceAdmissionError> {
        self.reopen_with(lease, |reopen| {
            reopen.commit();
            Ok::<_, ()>(())
        })
        .map(|settled| {
            settled.expect("an unconditional commit settles the fence");
        })
    }
    /// Validate exact canonical custody before taking this own gate. The
    /// callback may hold its actual publication compare guard through commit.
    /// No issuer/ancestor gate may be acquired inside it. Error, panic or a
    /// missing commit leaves the fence closed. Commit must be its last action.
    /// Borrow cleanup custody: dropping a last source claim or lexical owner in
    /// the callback, including during unwind, can reacquire these held locks.
    /// Success reopens only this gate; future work admits the full source chain.
    pub fn reopen_with<E>(
        self,
        lease: &Arc<super::RuntimeLexicalScopeLease>,
        settle: impl for<'a> FnOnce(RuntimeSourceReopen<'a>) -> Result<(), E>,
    ) -> Result<Result<OwnSourceFenceReopened, E>, SourceAdmissionError> {
        if !lease.source_admission().same_owner(&self.owner) {
            return Err(SourceAdmissionError::StaleFence);
        }
        super::RuntimeLexicalScopeLease::with_source_leases([lease.clone()], |_| {
            let mut state = self.owner.0.state.lock();
            let original = SourceAdmissionPhase::Fenced {
                attempt: self.attempt,
            };
            if state.phase != original {
                return Err(SourceAdmissionError::StaleFence);
            }
            if state.unconfirmed != 0 {
                return Err(SourceAdmissionError::Unconfirmed {
                    scope: self.owner.0.scope,
                    count: state.unconfirmed,
                });
            }
            let mut scoped = ScopedReopen {
                state: &mut state,
                original,
                committed: false,
                accepted: false,
            };
            let result = settle(RuntimeSourceReopen {
                state: &mut *scoped.state,
                committed: &mut scoped.committed,
            });
            match result {
                Ok(()) if scoped.committed => {
                    scoped.accepted = true;
                    Ok(Ok(OwnSourceFenceReopened { _private: () }))
                }
                Ok(_) => Err(SourceAdmissionError::ReopenNotCommitted),
                Err(error) => Ok(Err(error)),
            }
        })
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
    fn publication_view_requires_its_exact_still_held_fence() {
        let mut session = PersistentSession::new(None, 0);
        let lease = session.mint_notebook_scope_with_lease(None).unwrap();
        let other = session.mint_notebook_scope_with_lease(None).unwrap();
        let fence = lease.source_admission().fence().unwrap();
        let view = fence.publication_view();
        let ran = std::cell::Cell::new(false);
        assert_eq!(
            view.with_current(&other, || ran.set(true)),
            Err(SourceAdmissionError::StaleFence)
        );
        assert!(!ran.get());
        view.with_current(&lease, || ran.set(true)).unwrap();
        assert!(ran.replace(false));

        fence.reopen(&lease).unwrap();
        assert_eq!(
            view.with_current(&lease, || ran.set(true)),
            Err(SourceAdmissionError::StaleFence)
        );
        let replacement = lease.source_admission().fence().unwrap();
        assert_eq!(
            view.with_current(&lease, || ran.set(true)),
            Err(SourceAdmissionError::StaleFence)
        );
        assert!(!ran.get());
        replacement
            .publication_view()
            .with_current(&lease, || ran.set(true))
            .unwrap();
        assert!(ran.get());
    }

    #[test]
    fn publication_view_keeps_pending_and_unconfirmed_source_obligations() {
        for unconfirmed in [false, true] {
            let mut session = PersistentSession::new(None, 0);
            let lease = session.mint_notebook_scope_with_lease(None).unwrap();
            let claim = lease.admit_source().unwrap();
            let fence = lease.source_admission().fence().unwrap();
            let view = fence.publication_view();
            if unconfirmed {
                claim.retain_unconfirmed();
                assert!(matches!(
                    view.with_current(&lease, || panic!("unconfirmed source cannot publish")),
                    Err(SourceAdmissionError::Unconfirmed { count: 1, .. })
                ));
                assert_eq!(fence.unconfirmed_claims(), 1);
            } else {
                assert!(matches!(
                    view.with_current(&lease, || panic!("pending source cannot publish")),
                    Err(SourceAdmissionError::BootstrapBusy {
                        claims: 1,
                        unconfirmed: 0
                    })
                ));
                drop(claim);
                view.with_current(&lease, || ()).unwrap();
            }
        }
    }

    #[test]
    fn scoped_cutoff_refusal_and_missing_commit_leave_gate_unchanged() {
        let mut session = PersistentSession::new(None, 0);
        let lease = session.mint_notebook_scope_with_lease(None).unwrap();
        let gate = lease.source_admission();
        assert!(matches!(
            gate.fence_with(&lease, |_| Err("selection changed")),
            Ok(Err(SourceFenceTransitionFailure::Refused(
                "selection changed"
            )))
        ));
        assert_eq!(gate.0.state.lock().next_attempt, 0);
        assert!(lease.admit_source().is_ok());
        assert!(matches!(
            gate.fence_with(&lease, |_| Ok::<_, ()>(())),
            Err(SourceAdmissionError::FenceNotCommitted)
        ));
        assert_eq!(gate.0.state.lock().next_attempt, 0);
        assert!(lease.admit_source().is_ok());
    }

    #[test]
    fn scoped_cutoff_holds_gate_through_owner_compare_and_retains_active_claims() {
        let mut session = PersistentSession::new(None, 0);
        let lease = session.mint_notebook_scope_with_lease(None).unwrap();
        let gate = lease.source_admission();
        let claim = lease.admit_source().unwrap();
        let fence = match gate.fence_with(&lease, |transition| {
            assert!(gate.0.state.try_lock().is_none());
            transition.commit();
            Ok::<_, ()>(())
        }) {
            Ok(Ok(fence)) => fence,
            _ => panic!("the actual owner committed its cutoff"),
        };
        assert_eq!(fence.pending_claims(), 1);
        assert!(lease.admit_source().is_err());
        drop(claim);
        fence.reopen(&lease).unwrap();
        assert!(lease.admit_source().is_ok());
    }

    #[test]
    fn scoped_cutoff_failed_after_commit_returns_same_fence_and_primary_cause() {
        let mut session = PersistentSession::new(None, 0);
        let lease = session.mint_notebook_scope_with_lease(None).unwrap();
        let gate = lease.source_admission();
        let fence = match gate.fence_with(&lease, |transition| {
            transition.commit();
            Err("owner completion failed")
        }) {
            Ok(Err(SourceFenceTransitionFailure::Unconfirmed { fence, error })) => {
                assert_eq!(error, "owner completion failed");
                assert!(fence.owner.same_owner(gate));
                assert_eq!(fence.attempt, 0);
                fence
            }
            _ => panic!("a committed cutoff must retain its exact owner"),
        };
        assert!(lease.admit_source().is_err());
        fence.reopen(&lease).unwrap();
        assert!(lease.admit_source().is_ok());
    }

    #[test]
    fn scoped_cutoff_unwind_before_or_after_commit_never_reopens_admission() {
        for commit in [false, true] {
            let mut session = PersistentSession::new(None, 0);
            let lease = session.mint_notebook_scope_with_lease(None).unwrap();
            let gate = lease.source_admission();
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = gate.fence_with::<()>(&lease, |transition| {
                    if commit {
                        transition.commit();
                    }
                    panic!("cutoff owner unwound");
                });
            }))
            .is_err());
            assert!(matches!(
                gate.0.state.lock().phase,
                SourceAdmissionPhase::Fenced { attempt: 0 }
            ));
            assert_eq!(gate.0.state.lock().next_attempt, 1);
            assert!(lease.admit_source().is_err());
        }
    }

    #[test]
    fn conditional_reopen_commits_under_compare_guard_but_preserves_parent_fence() {
        let mut session = PersistentSession::new(None, 1024);
        let parent = session.mint_notebook_scope_with_lease(None).unwrap();
        let child = session
            .mint_notebook_scope_with_lease(Some(parent.clone()))
            .unwrap();
        let parent_fence = parent.source_admission().fence().unwrap();
        let child_fence = child.source_admission().fence().unwrap();
        let selected = Mutex::new(false);
        let _own_only = child_fence
            .reopen_with(&child, |reopen| {
                let mut current = selected.lock();
                *current = true;
                reopen.commit();
                Ok::<_, ()>(())
            })
            .unwrap()
            .unwrap();
        assert!(*selected.lock());
        assert!(matches!(
            child.admit_source(),
            Err(SourceAdmissionError::Closed { .. })
        ));
        parent_fence.reopen(&parent).unwrap();
        child.admit_source().unwrap();
    }

    #[test]
    fn conditional_reopen_missing_commit_error_and_unwind_keep_fence_closed() {
        let mut session = PersistentSession::new(None, 1024);
        for outcome in 0..4 {
            let scope = session.mint_notebook_scope_with_lease(None).unwrap();
            let fence = scope.source_admission().fence().unwrap();
            let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fence.reopen_with(&scope, |reopen| {
                    if outcome != 0 {
                        reopen.commit();
                    }
                    match outcome {
                        0 => Ok(()),
                        1 => Err("owner refused"),
                        2 => panic!("owner unwound after commit"),
                        _ => Ok(()),
                    }
                })
            }));
            match outcome {
                0 => assert!(matches!(
                    observed.unwrap(),
                    Err(SourceAdmissionError::ReopenNotCommitted)
                )),
                1 => assert!(matches!(observed.unwrap(), Ok(Err("owner refused")))),
                2 => assert!(observed.is_err()),
                _ => assert!(observed.unwrap().unwrap().is_ok()),
            }
            assert_eq!(scope.admit_source().is_ok(), outcome == 3);
        }
    }

    #[test]
    fn conditional_reopen_refuses_wrong_canonical_owner_and_retired_issuer() {
        let mut session = PersistentSession::new(None, 1024);
        let first = session.mint_notebook_scope_with_lease(None).unwrap();
        let other = session.mint_notebook_scope_with_lease(None).unwrap();
        let fence = first.source_admission().fence().unwrap();
        let called = std::cell::Cell::new(false);
        assert!(matches!(
            fence.reopen_with(&other, |reopen| {
                called.set(true);
                reopen.commit();
                Ok::<_, ()>(())
            }),
            Err(SourceAdmissionError::StaleFence)
        ));
        assert!(!called.get());
        assert!(first.admit_source().is_err());
        let fence = other.source_admission().fence().unwrap();
        drop(session);
        assert!(matches!(
            fence.reopen_with(&other, |reopen| {
                called.set(true);
                reopen.commit();
                Ok::<_, ()>(())
            }),
            Err(SourceAdmissionError::StaleSourceLease)
        ));
        assert!(!called.get());
    }

    #[test]
    fn unconditional_reopen_requires_same_owner_and_live_issuer() {
        let mut session = PersistentSession::new(None, 1024);
        let first = session.mint_notebook_scope_with_lease(None).unwrap();
        let other = session.mint_notebook_scope_with_lease(None).unwrap();
        let fence = first.source_admission().fence().unwrap();
        assert!(matches!(
            fence.reopen(&other),
            Err(SourceAdmissionError::StaleFence)
        ));
        assert!(first.source_admission().admit().is_err());
        let fence = other.source_admission().fence().unwrap();
        drop(session);
        assert!(matches!(
            fence.reopen(&other),
            Err(SourceAdmissionError::StaleSourceLease)
        ));
        assert!(other.source_admission().admit().is_err());
    }

    #[test]
    fn conditional_reopen_cannot_discard_unconfirmed_source_work() {
        let mut session = PersistentSession::new(None, 1024);
        let scope = session.mint_notebook_scope_with_lease(None).unwrap();
        let claim = scope.admit_source().unwrap();
        let fence = scope.source_admission().fence().unwrap();
        claim.retain_unconfirmed();
        let called = std::cell::Cell::new(false);
        assert!(matches!(
            fence.reopen_with(&scope, |reopen| {
                called.set(true);
                reopen.commit();
                Ok::<_, ()>(())
            }),
            Err(SourceAdmissionError::Unconfirmed { count: 1, .. })
        ));
        assert!(!called.get());
        assert!(scope.admit_source().is_err());
    }

    #[test]
    fn fencing_source_refuses_late_binding_without_retiring_custody() {
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let scope = session.mint_isolated_scope_with_lease().unwrap();
        let mut before = session
            .with_source_compiler_transaction(
                scope.clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| {
                    crate::session::prepared::tests::rooted_publication_fixture(
                        session, "before", 70,
                    )
                },
            )
            .unwrap()
            .into_outcome()
            .action;
        before.scope = scope.scope();
        session
            .with_source_compiler_transaction(
                scope.clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, report| {
                    session.bind_replacing_decls_in(scope.scope(), vec![before], report)
                },
            )
            .unwrap()
            .into_outcome()
            .action
            .unwrap();
        let mut late = session
            .with_source_compiler_transaction(
                scope.clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| {
                    crate::session::prepared::tests::rooted_publication_fixture(session, "late", 71)
                },
            )
            .unwrap()
            .into_outcome()
            .action;
        late.scope = scope.scope();
        let before_handles = session.value_handle_count();
        let fence = scope.source_admission().fence().unwrap();
        assert!(matches!(
            session
                .with_source_compiler_transaction(
                    scope.clone(),
                    tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                    crate::CompilerTransactionCancellation::new(),
                    None,
                    |_| {},
                    |session, report| session.bind_replacing_decls_in(
                        scope.scope(),
                        vec![late],
                        report
                    )
                )
                .unwrap()
                .into_outcome()
                .action,
            Err(SessionError::SourceAdmission(
                SourceAdmissionError::Closed { .. }
            ))
        ));
        assert!(session.resolve_in(scope.scope(), "before").is_some());
        assert!(session.resolve_in(scope.scope(), "late").is_none());
        assert_eq!(session.value_handle_count() + 1, before_handles);
        assert!(session.scope_tree().is_live(scope.scope()));
        fence.reopen(&scope).unwrap();
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
            fence.reopen(&scope),
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
            .adopt_initial_notebook_scope_with_source(inherited, Some(parent.clone()))
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
            let child = session.mint_notebook_scope_with_lease(Some(parent.clone())).unwrap();
            let gates = [parent.source_admission(), child.source_admission()];
            let leases = [&parent, &child];
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
                        let result = fence.reopen(leases[gate]);
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

/// One physical compiler scope retains every earned lower claim until its END.
/// The original lease is independent of the recipient's current placement.
pub struct RuntimeSourceCompilerTransaction {
    pub(super) source: Arc<super::RuntimeOriginalSource>,
    operation_target: Option<Arc<super::RuntimeLexicalScopeLease>>,
    origin: crate::CompilerTransactionOrigin,
    hook: Option<SourceAdmissionHook>,
    holds: Mutex<SourceCompilerHolds>,
    closed: std::sync::atomic::AtomicBool,
}
#[derive(Default)]
struct SourceCompilerHolds {
    hook_admitted: bool,
    claims: Vec<(RuntimeSourceAdmission, RuntimeSourceClaim)>,
    sources: Vec<Arc<super::RuntimeOriginalSource>>,
    leases: Vec<Arc<super::RuntimeLexicalScopeLease>>,
}
impl RuntimeSourceCompilerTransaction {
    pub(super) fn origin(&self) -> &crate::CompilerTransactionOrigin {
        &self.origin
    }

    pub(crate) fn admit_original(
        &self,
        original: &Arc<super::RuntimeOriginalSource>,
        target: Option<Arc<super::RuntimeLexicalScopeLease>>,
    ) -> Result<(), SourceAdmissionError> {
        if !Arc::ptr_eq(&self.source, original) {
            return Err(SourceAdmissionError::ForeignTransaction);
        }
        self.admit(target)
    }

    pub(super) fn admit(
        &self,
        target: Option<Arc<super::RuntimeLexicalScopeLease>>,
    ) -> Result<(), SourceAdmissionError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire)
            || crate::CompilerTransactionOrigin::current().as_ref() != Some(&self.origin)
        {
            return Err(SourceAdmissionError::ForeignTransaction);
        }
        self.source.validate_source_issuer()?;
        let mut holds = self.holds.lock();
        if !holds.hook_admitted {
            if let Some(hook) = &self.hook {
                hook()?;
            }
            holds.hook_admitted = true;
        }
        super::RuntimeOriginalSource::with_sources_and_targets(
            [self.source.clone()],
            self.operation_target.iter().cloned().chain(target),
            |sources, leases| {
                for source in sources {
                    let admission = source.source_admission();
                    if !holds
                        .claims
                        .iter()
                        .any(|(held, _)| held.same_owner(admission))
                    {
                        holds
                            .claims
                            .push((admission.clone(), admission.admit_local()?));
                    }
                    if !holds.sources.iter().any(|held| Arc::ptr_eq(held, source)) {
                        holds.sources.push(source.clone());
                    }
                }
                for lease in leases {
                    if !holds.leases.iter().any(|held| Arc::ptr_eq(held, lease)) {
                        holds.leases.push(lease.clone());
                    }
                }
                Ok(())
            },
        )
    }
    pub(crate) fn admitted_original_source(
        &self,
    ) -> Result<Arc<super::RuntimeOriginalSource>, SourceAdmissionError> {
        self.admit(None)?;
        Ok(self.source.clone())
    }

    fn settle(&self, confirmed: bool) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        let mut holds = self.holds.lock();
        if !confirmed {
            for source in &holds.sources {
                source.retain_unconfirmed_source_custody();
            }
            for lease in &holds.leases {
                lease.retain_unconfirmed_source_custody();
            }
        }
        for (_, claim) in holds.claims.drain(..) {
            if !confirmed {
                claim.retain_unconfirmed();
            }
        }
        holds.sources.clear();
        holds.leases.clear();
    }
}

pub(super) fn run_source_compiler_transaction<T>(
    source: Arc<super::RuntimeOriginalSource>,
    operation_target: Option<Arc<super::RuntimeLexicalScopeLease>>,
    workload: tidepool_toolchain::artifacts::CompileWorkload,
    cancellation: crate::CompilerTransactionCancellation,
    supplemental_hook: Option<SourceAdmissionHook>,
    close_sink: impl FnOnce(crate::CompilerTransactionClose),
    action: impl FnOnce(
        Arc<RuntimeSourceCompilerTransaction>,
        &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> T,
) -> Result<crate::CompilerTransactionSettled<T>, SourceAdmissionError> {
    if let Err(error) = source.validate_source_issuer() {
        close_sink(crate::CompilerTransactionClose::NotStarted);
        return Err(error);
    }
    if crate::CompilerTransactionOrigin::current().is_some() {
        close_sink(crate::CompilerTransactionClose::NotStarted);
        return Err(SourceAdmissionError::ForeignTransaction);
    }
    let context = std::cell::RefCell::new(None::<Arc<RuntimeSourceCompilerTransaction>>);
    let returned = std::cell::Cell::new(false);
    Ok(crate::settle_compiler_transaction_cancellable_for_workload(
        workload,
        cancellation,
        |close| {
            if let Some(context) = context.borrow().as_ref() {
                context.settle(returned.get() && close.resources_retired());
            }
            close_sink(close);
        },
        |report| {
            let issued = Arc::new(RuntimeSourceCompilerTransaction {
                source,
                operation_target,
                origin: crate::CompilerTransactionOrigin::current().expect("actual compiler owner"),
                hook: supplemental_hook,
                holds: Mutex::new(SourceCompilerHolds::default()),
                closed: std::sync::atomic::AtomicBool::new(false),
            });
            *context.borrow_mut() = Some(issued.clone());
            let value = action(issued, report);
            returned.set(true);
            value
        },
    ))
}

impl super::RuntimeOriginalSource {
    /// Run one off-checkout compiler phase from its actual original-source token.
    /// The borrowed capability can only be used inside this physical owner.
    pub fn with_source_compiler_transaction<T>(
        self: &Arc<Self>,
        workload: tidepool_toolchain::artifacts::CompileWorkload,
        cancellation: crate::CompilerTransactionCancellation,
        supplemental_hook: Option<SourceAdmissionHook>,
        close_sink: impl FnOnce(crate::CompilerTransactionClose),
        action: impl FnOnce(
            &RuntimeSourceCompilerTransaction,
            &mut dyn FnMut(crate::CompilerTransactionClose),
        ) -> T,
    ) -> Result<crate::CompilerTransactionSettled<T>, SourceAdmissionError> {
        run_source_compiler_transaction(
            self.clone(),
            None,
            workload,
            cancellation,
            supplemental_hook,
            close_sink,
            |owner, report| action(&owner, report),
        )
    }
}

impl super::RuntimeLexicalScopeLease {
    /// Retain this actual operation placement separately from source identity.
    pub fn with_source_compiler_transaction<T>(
        self: &Arc<Self>,
        workload: tidepool_toolchain::artifacts::CompileWorkload,
        cancellation: crate::CompilerTransactionCancellation,
        supplemental_hook: Option<SourceAdmissionHook>,
        close_sink: impl FnOnce(crate::CompilerTransactionClose),
        action: impl FnOnce(
            &RuntimeSourceCompilerTransaction,
            &mut dyn FnMut(crate::CompilerTransactionClose),
        ) -> T,
    ) -> Result<crate::CompilerTransactionSettled<T>, SourceAdmissionError> {
        run_source_compiler_transaction(
            self.original_source().clone(),
            Some(self.clone()),
            workload,
            cancellation,
            supplemental_hook,
            close_sink,
            |owner, report| action(&owner, report),
        )
    }
}
