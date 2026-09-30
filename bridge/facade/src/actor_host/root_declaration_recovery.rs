//! The production root's declaration graph stays under the retained host run
//! lease. Runtime owns its codec, protected hydration and manifest publication.

use super::HostIncarnationLease;
use std::path::Path;
use std::sync::Arc;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    RecoveryPublicOwner, RecoveryRunAuthority, RecoverySuccessorAuthority, SessionError, SessionLib,
};

struct RootDeclarationRunAuthority {
    lease: Arc<HostIncarnationLease>,
}

impl RecoveryRunAuthority for RootDeclarationRunAuthority {
    fn owns_run(&self, run_root: &Path) -> std::io::Result<bool> {
        self.lease.owns_run(run_root)
    }
}

struct RootDeclarationSuccessorAuthority {
    lease: Arc<HostIncarnationLease>,
    admission: Arc<exomonad_actor::DurableRootSuccessorAdmission>,
}

impl RecoverySuccessorAuthority for RootDeclarationSuccessorAuthority {
    fn validate_successor(
        &self,
        run_root: &Path,
        predecessor: &RecoveryPublicOwner,
        successor: &RecoveryPublicOwner,
        session: SessionId,
        target: ScopeId,
    ) -> std::io::Result<bool> {
        Ok(self.lease.owns_run(run_root)?
            && self.admission.validate_successor(
                run_root,
                predecessor,
                successor,
                session,
                target,
            )?)
    }
}

pub(super) fn successor_authority(
    lease: Arc<HostIncarnationLease>,
    admission: Arc<exomonad_actor::DurableRootSuccessorAdmission>,
) -> Arc<dyn RecoverySuccessorAuthority> {
    Arc::new(RootDeclarationSuccessorAuthority { lease, admission })
}

pub(super) fn root_path() -> tidepool_repr::ActorPath {
    tidepool_repr::ActorPath::parse("root").expect("canonical root actor path")
}

pub(super) fn attach(
    library: &mut SessionLib,
    run_root: &Path,
    lease: Arc<HostIncarnationLease>,
) -> Result<(), SessionError> {
    library.attach_owned_recovery_graph_v3(
        run_root.join("root-declarations.json"),
        Arc::new(RootDeclarationRunAuthority { lease }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_runtime::session::ModuleEnv;

    fn library(root: &Path) -> SessionLib {
        SessionLib::open(
            tidepool_runtime::session::fresh_session_id(),
            root.join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
    }

    #[test]
    fn root_graph_retains_the_actual_host_lease() {
        let run = tempfile::tempdir().unwrap();
        let lease = Arc::new(HostIncarnationLease::claim(run.path()).unwrap());
        let mut library = library(run.path());
        attach(&mut library, run.path(), Arc::clone(&lease)).unwrap();
        drop(lease);
        assert!(HostIncarnationLease::claim(run.path()).is_err());
        drop(library);
        assert!(HostIncarnationLease::claim(run.path()).is_ok());
    }

    #[test]
    fn root_graph_refuses_a_lease_for_another_run() {
        let owned = tempfile::tempdir().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        let lease = Arc::new(HostIncarnationLease::claim(owned.path()).unwrap());
        let mut library = library(unrelated.path());
        assert!(attach(&mut library, unrelated.path(), lease).is_err());
        assert!(!unrelated.path().join("root-declarations.json").exists());
    }

    #[test]
    fn root_graph_refuses_legacy_source_replay_and_preserves_evidence() {
        let run = tempfile::tempdir().unwrap();
        let path = run.path().join("root-declarations.json");
        let legacy = br#"{"version":1,"source_session":41,"turns":[]}"#;
        std::fs::write(&path, legacy).unwrap();
        let lease = Arc::new(HostIncarnationLease::claim(run.path()).unwrap());
        let mut library = library(run.path());
        assert!(attach(&mut library, run.path(), lease).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
    }
    #[test]
    fn cold_root_readback_requires_the_exact_persisted_actor_incarnation() {
        use tidepool_runtime::session::{PersistentSession, PublicManifestCommit};
        let run = tempfile::tempdir().unwrap();
        let first_lease = Arc::new(HostIncarnationLease::claim(run.path()).unwrap());
        let first_owner =
            RecoveryPublicOwner::new(&root_path(), first_lease.incarnation().0).unwrap();
        let mut first_library = library(run.path());
        attach(&mut first_library, run.path(), Arc::clone(&first_lease)).unwrap();
        let mut first = PersistentSession::new(Some(first_library), 1024);
        let public = first.mint_isolated_scope();
        assert_eq!(
            first
                .initialize_durable_public_scope(first_owner.clone(), public)
                .unwrap(),
            PublicManifestCommit::Durable,
        );
        let private = first.begin_private_execution(public).unwrap();
        let intent = first
            .freeze_execution_intent(&private, vec![], vec![])
            .unwrap();
        drop(first);
        drop(first_lease);

        let successor_lease = Arc::new(HostIncarnationLease::claim(run.path()).unwrap());
        let successor_owner =
            RecoveryPublicOwner::new(&root_path(), successor_lease.incarnation().0).unwrap();
        assert_ne!(first_owner, successor_owner);
        let mut successor_library = library(run.path());
        attach(&mut successor_library, run.path(), successor_lease).unwrap();
        let mut successor = PersistentSession::new(Some(successor_library), 1024);
        assert!(successor.recover_public_scope(&successor_owner).is_err());
        let recovered = successor.recover_public_scope(&first_owner).unwrap();
        let snapshot = successor.public_visibility_snapshot_in(recovered).unwrap();
        assert!(snapshot.bindings.is_empty());
        assert!(snapshot.source_instances.is_empty());
        assert!(successor
            .restage_execution_publication(first_owner, intent)
            .is_err());
    }
}
