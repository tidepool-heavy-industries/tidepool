//! The production root's declaration graph stays under the retained host run
//! lease. Runtime owns its codec, protected hydration and manifest publication.

use super::HostIncarnationLease;
use std::path::{Path, PathBuf};
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

struct ChildDeclarationRunAuthority {
    lease: Arc<HostIncarnationLease>,
    host_root: PathBuf,
    child_root: PathBuf,
}

impl RecoveryRunAuthority for ChildDeclarationRunAuthority {
    fn owns_run(&self, run_root: &Path) -> std::io::Result<bool> {
        Ok(run_root == self.child_root && self.lease.owns_run(&self.host_root)?)
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
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<(), SessionError> {
    library.attach_owned_recovery_graph_v3(
        run_root.join("root-declarations.json"),
        Arc::new(RootDeclarationRunAuthority { lease }),
        settlement,
    )
}

/// A child owns its graph under its exact session directory while the retained
/// host lease continues to own the enclosing run.
pub(super) fn attach_child(
    library: &mut SessionLib,
    host_root: &Path,
    lease: Arc<HostIncarnationLease>,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<(), SessionError> {
    let host_root = host_root
        .canonicalize()
        .map_err(|error| SessionError::RecoveryManifest {
            path: host_root.to_path_buf(),
            detail: error.to_string(),
        })?;
    let child_root = host_root
        .join("haskell-session-children")
        .join(library.session_id().0.to_string());
    let actual_root =
        library
            .include_dir()
            .canonicalize()
            .map_err(|error| SessionError::RecoveryManifest {
                path: library.include_dir().to_path_buf(),
                detail: error.to_string(),
            })?;
    if actual_root != child_root {
        return Err(SessionError::RecoveryManifest {
            path: actual_root,
            detail: "child declaration graph requires its exact session directory".into(),
        });
    }
    library.attach_owned_recovery_graph_v3(
        child_root.join("declarations.json"),
        Arc::new(ChildDeclarationRunAuthority {
            lease,
            host_root,
            child_root,
        }),
        settlement,
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
    fn child_graphs_keep_independent_high_water_under_the_same_host_lease() {
        let run = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut root_library = library(run.path());
        tidepool_testing::with_settlement(|settlement| {
            attach(&mut root_library, run.path(), lease.clone(), settlement)
        })
        .unwrap();
        root_library
            .initialize_captured_declaration_high_water(tidepool_repr::Generation(3))
            .unwrap();
        let root_bytes = std::fs::read(run.path().join("root-declarations.json")).unwrap();
        let mut paths = Vec::new();
        for high_water in [4, 7] {
            let id = tidepool_runtime::session::fresh_session_id();
            let child_root = run
                .path()
                .join("haskell-session-children")
                .join(id.0.to_string());
            let mut child =
                SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
            tidepool_testing::with_settlement(|settlement| {
                attach_child(&mut child, run.path(), lease.clone(), settlement)
            })
            .unwrap();
            assert_eq!(child.generation(), tidepool_repr::Generation(0));
            child
                .initialize_captured_declaration_high_water(tidepool_repr::Generation(high_water))
                .unwrap();
            let path = child_root.join("declarations.json");
            let graph: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(graph["source_session"], id.0);
            assert_eq!(graph["high_water"], high_water);
            paths.push(path);
        }
        assert_ne!(paths[0], paths[1]);
        assert_eq!(
            std::fs::read(run.path().join("root-declarations.json")).unwrap(),
            root_bytes
        );
    }

    #[test]
    fn child_graph_refuses_wrong_host_lease_and_another_session_directory() {
        let owned = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(owned.path()).unwrap(),
            )
            .unwrap(),
        );
        let id = tidepool_runtime::session::fresh_session_id();
        let child_root = foreign
            .path()
            .join("haskell-session-children")
            .join(id.0.to_string());
        let mut child = SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
        assert!(tidepool_testing::with_settlement(|settlement| attach_child(
            &mut child,
            foreign.path(),
            lease.clone(),
            settlement
        ))
        .is_err());
        assert!(!child_root.join("declarations.json").exists());
        let wrong_root = owned
            .path()
            .join("haskell-session-children")
            .join(id.0.to_string());
        let mut wrong = SessionLib::open(
            tidepool_runtime::session::fresh_session_id(),
            &wrong_root,
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        assert!(tidepool_testing::with_settlement(|settlement| attach_child(
            &mut wrong,
            owned.path(),
            lease,
            settlement
        ))
        .is_err());
        assert!(!wrong_root.join("declarations.json").exists());
    }

    #[test]
    fn root_graph_retains_the_actual_host_lease() {
        let run = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut library = library(run.path());
        tidepool_testing::with_settlement(|settlement| {
            attach(&mut library, run.path(), Arc::clone(&lease), settlement)
        })
        .unwrap();
        drop(lease);
        assert!(HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap()
        )
        .is_err());
        drop(library);
        assert!(HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap()
        )
        .is_ok());
    }

    #[test]
    fn root_graph_refuses_a_lease_for_another_run() {
        let owned = tempfile::tempdir().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(owned.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut library = library(unrelated.path());
        assert!(tidepool_testing::with_settlement(|settlement| attach(
            &mut library,
            unrelated.path(),
            lease,
            settlement
        ))
        .is_err());
        assert!(!unrelated.path().join("root-declarations.json").exists());
    }

    #[test]
    fn root_graph_refuses_legacy_source_replay_and_preserves_evidence() {
        let run = tempfile::tempdir().unwrap();
        let path = run.path().join("root-declarations.json");
        let legacy = br#"{"version":1,"source_session":41,"turns":[]}"#;
        std::fs::write(&path, legacy).unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut library = library(run.path());
        assert!(tidepool_testing::with_settlement(|settlement| attach(
            &mut library,
            run.path(),
            lease,
            settlement
        ))
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
    }
    #[test]
    fn cold_root_readback_requires_the_exact_persisted_actor_incarnation() {
        use tidepool_runtime::session::{PersistentSession, PublicManifestCommit};
        let run = tempfile::tempdir().unwrap();
        let first_lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let first_owner =
            RecoveryPublicOwner::new(&root_path(), first_lease.incarnation().0).unwrap();
        let mut first_library = library(run.path());
        tidepool_testing::with_settlement(|settlement| {
            attach(
                &mut first_library,
                run.path(),
                Arc::clone(&first_lease),
                settlement,
            )
        })
        .unwrap();
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

        let successor_lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let successor_owner =
            RecoveryPublicOwner::new(&root_path(), successor_lease.incarnation().0).unwrap();
        assert_ne!(first_owner, successor_owner);
        let mut successor_library = library(run.path());
        tidepool_testing::with_settlement(|settlement| {
            attach(
                &mut successor_library,
                run.path(),
                successor_lease,
                settlement,
            )
        })
        .unwrap();
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
