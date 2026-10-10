//! Recovery fixtures use the same held-run-lock and domain attachment contract as hosts.

use super::recovery_store::{RecoveryDomainId, RecoveryDomainState, RunRecoveryStore};
use super::{RecoveryRunAuthority, SessionLib};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct TestRunOwner {
    root: PathBuf,
    _lock: std::fs::File,
}

impl RecoveryRunAuthority for TestRunOwner {
    fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
        Ok(root.canonicalize()? == self.root)
    }
}

pub(super) fn run_owner(root: &Path) -> Arc<TestRunOwner> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("test-run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    Arc::new(TestRunOwner {
        root: root.canonicalize().unwrap(),
        _lock: lock,
    })
}

pub(super) struct RecoveryFixture {
    pub(super) owner: Arc<TestRunOwner>,
    pub(super) store: Arc<RunRecoveryStore>,
}

impl RecoveryFixture {
    pub(super) fn create(root: &Path) -> Self {
        let owner = run_owner(root);
        assert!(owner.owns_run(root).unwrap());
        let store =
            Arc::new(RunRecoveryStore::create(&owner.root.join("recovery.sqlite")).unwrap());
        Self { owner, store }
    }

    pub(super) fn reopen(owner: Arc<TestRunOwner>) -> Self {
        assert!(owner.owns_run(&owner.root).unwrap());
        let store = Arc::new(RunRecoveryStore::open(&owner.root.join("recovery.sqlite")).unwrap());
        Self { owner, store }
    }

    pub(super) fn attach_root(
        &self,
        lib: &mut SessionLib,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), super::SessionError> {
        self.attach(lib, RecoveryDomainId::Root, settlement)
    }

    pub(super) fn attach(
        &self,
        lib: &mut SessionLib,
        domain: RecoveryDomainId,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), super::SessionError> {
        let artifact_root = lib.include_dir().canonicalize()?;
        lib.attach_owned_recovery_domain(
            self.store.clone(),
            domain,
            artifact_root,
            self.owner.clone(),
            settlement,
        )
    }

    pub(super) fn state(&self) -> RecoveryDomainState {
        self.store
            .read_domain(RecoveryDomainId::Root)
            .unwrap()
            .unwrap()
    }
}
