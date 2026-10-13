//! The production root's declaration graph stays under the retained host run
//! lease. Runtime owns domain hydration and atomic root publication.

use super::HostIncarnationLease;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    RecoveryDomainId, RecoveryPublicOwner, RecoveryRunAuthority, RecoverySuccessorAuthority,
    RunRecoveryStore, SessionError, SessionLib,
};

struct ChildDeclarationRunAuthority {
    lease: Arc<HostIncarnationLease>,
    host_root: PathBuf,
    child_root: PathBuf,
}

impl RecoveryRunAuthority for ChildDeclarationRunAuthority {
    fn owns_run(&self, run_root: &Path) -> std::io::Result<bool> {
        Ok(run_root == self.host_root
            && self.child_root.canonicalize()? == self.child_root
            && self.lease.owns_run(&self.host_root)?)
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

pub(super) fn store_path(run_root: &Path) -> PathBuf {
    run_root.join("recovery.sqlite")
}

pub(super) fn attach(
    library: &mut SessionLib,
    run_root: &Path,
    lease: Arc<HostIncarnationLease>,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<Arc<RunRecoveryStore>, SessionError> {
    let run_root = run_root
        .canonicalize()
        .map_err(|error| SessionError::RecoveryManifest {
            path: run_root.to_path_buf(),
            detail: error.to_string(),
        })?;
    let invalid = |detail: String| SessionError::RecoveryManifest {
        path: run_root.clone(),
        detail,
    };
    if !lease
        .owns_run(&run_root)
        .map_err(|error| invalid(error.to_string()))?
    {
        return Err(invalid(
            "root recovery requires its exact host run lease".into(),
        ));
    }
    let legacy = run_root.join("root-declarations.json");
    if legacy
        .try_exists()
        .map_err(|error| invalid(error.to_string()))?
    {
        return Err(invalid(
            "legacy declaration recovery requires its matching binary".into(),
        ));
    }
    let path = store_path(&run_root);
    let store = if path
        .try_exists()
        .map_err(|error| invalid(error.to_string()))?
    {
        RunRecoveryStore::open(&path)
    } else {
        RunRecoveryStore::create(&path)
    }
    .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
    let store = Arc::new(store);
    library.attach_owned_recovery_domain(
        Arc::clone(&store),
        RecoveryDomainId::Root,
        library.include_dir().to_path_buf(),
        lease,
        settlement,
    )?;
    Ok(store)
}

/// An independent child shares the run database while retaining its exact
/// compiler artifact directory and its own machine-session domain.
pub(super) fn attach_child(
    library: &mut SessionLib,
    host_root: &Path,
    store: Arc<RunRecoveryStore>,
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
    if store.path() != store_path(&host_root) {
        return Err(SessionError::RecoveryManifest {
            path: store.path().to_path_buf(),
            detail: "child recovery store differs from its exact host run".into(),
        });
    }
    library.attach_owned_recovery_domain(
        store,
        RecoveryDomainId::Session(library.session_id()),
        child_root.clone(),
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
    fn child_domains_keep_independent_ceilings_under_the_same_host_lease() {
        let run = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut root_library = library(run.path());
        let store = tidepool_testing::with_settlement(|settlement| {
            attach(&mut root_library, run.path(), lease.clone(), settlement)
        })
        .unwrap();
        root_library
            .initialize_captured_declaration_high_water(tidepool_repr::Generation(3))
            .unwrap();
        let root_before = store.read_domain(RecoveryDomainId::Root).unwrap().unwrap();
        let mut domains = Vec::new();
        for ceiling in [4, 7] {
            let id = tidepool_runtime::session::fresh_session_id();
            let child_root = run
                .path()
                .join("haskell-session-children")
                .join(id.0.to_string());
            let mut child =
                SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
            tidepool_testing::with_settlement(|settlement| {
                attach_child(
                    &mut child,
                    run.path(),
                    Arc::clone(&store),
                    lease.clone(),
                    settlement,
                )
            })
            .unwrap();
            assert_eq!(child.generation(), tidepool_repr::Generation(0));
            child
                .initialize_captured_declaration_high_water(tidepool_repr::Generation(ceiling))
                .unwrap();
            let domain = RecoveryDomainId::Session(id);
            assert_eq!(
                store
                    .read_domain(domain)
                    .unwrap()
                    .unwrap()
                    .allocation_ceiling,
                tidepool_repr::Generation(ceiling)
            );
            assert!(!child_root.join("declarations.json").exists());
            domains.push(domain);
        }
        assert_ne!(domains[0], domains[1]);
        assert_eq!(
            store.read_domain(RecoveryDomainId::Root).unwrap().unwrap(),
            root_before
        );
    }

    #[test]
    fn child_domain_refuses_wrong_host_lease_and_another_session_directory() {
        let owned = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(owned.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut root_library = library(owned.path());
        let store = tidepool_testing::with_settlement(|settlement| {
            attach(
                &mut root_library,
                owned.path(),
                Arc::clone(&lease),
                settlement,
            )
        })
        .unwrap();
        let id = tidepool_runtime::session::fresh_session_id();
        let child_root = foreign
            .path()
            .join("haskell-session-children")
            .join(id.0.to_string());
        let mut child = SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
        assert!(tidepool_testing::with_settlement(|settlement| attach_child(
            &mut child,
            foreign.path(),
            Arc::clone(&store),
            lease.clone(),
            settlement
        ))
        .is_err());
        assert!(store
            .read_domain(RecoveryDomainId::Session(id))
            .unwrap()
            .is_none());
        let wrong_root = owned
            .path()
            .join("haskell-session-children")
            .join(id.0.to_string());
        let wrong_id = tidepool_runtime::session::fresh_session_id();
        let mut wrong =
            SessionLib::open(wrong_id, &wrong_root, ModuleEnv::standalone_default()).unwrap();
        assert!(tidepool_testing::with_settlement(|settlement| attach_child(
            &mut wrong,
            owned.path(),
            Arc::clone(&store),
            lease,
            settlement
        ))
        .is_err());
        assert!(store
            .read_domain(RecoveryDomainId::Session(wrong_id))
            .unwrap()
            .is_none());
    }

    #[test]
    fn child_domain_refuses_a_reused_machine_identity() {
        let run = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut root_library = library(run.path());
        let store = tidepool_testing::with_settlement(|settlement| {
            attach(
                &mut root_library,
                run.path(),
                Arc::clone(&lease),
                settlement,
            )
        })
        .unwrap();
        let id = tidepool_runtime::session::fresh_session_id();
        let child_root = run
            .path()
            .join("haskell-session-children")
            .join(id.0.to_string());
        let mut first = SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
        tidepool_testing::with_settlement(|settlement| {
            attach_child(
                &mut first,
                run.path(),
                Arc::clone(&store),
                Arc::clone(&lease),
                settlement,
            )
        })
        .unwrap();
        let before = store.read_domain(RecoveryDomainId::Session(id)).unwrap();
        let mut reused =
            SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
        assert!(tidepool_testing::with_settlement(|settlement| attach_child(
            &mut reused,
            run.path(),
            Arc::clone(&store),
            lease,
            settlement
        ))
        .is_err());
        assert_eq!(
            store.read_domain(RecoveryDomainId::Session(id)).unwrap(),
            before
        );
    }

    #[test]
    fn sibling_domain_publication_preserves_the_root_startup_pin() {
        use tidepool_runtime::session::PersistentSession;
        let run = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let mut root_library = library(run.path());
        let store = tidepool_testing::with_settlement(|settlement| {
            attach(
                &mut root_library,
                run.path(),
                Arc::clone(&lease),
                settlement,
            )
        })
        .unwrap();
        let owner = RecoveryPublicOwner::new(&root_path(), lease.incarnation().0).unwrap();
        let mut root = PersistentSession::new(Some(root_library), 1024);
        let public = root.mint_isolated_scope();
        root.initialize_durable_public_scope(owner.clone(), public)
            .unwrap();
        let before =
            exomonad_actor::RootStartupManifestPin::capture_for_actor(store.path(), &owner)
                .unwrap();
        let id = tidepool_runtime::session::fresh_session_id();
        let child_root = run
            .path()
            .join("haskell-session-children")
            .join(id.0.to_string());
        let mut child = SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
        tidepool_testing::with_settlement(|settlement| {
            attach_child(
                &mut child,
                run.path(),
                Arc::clone(&store),
                lease,
                settlement,
            )
        })
        .unwrap();
        child
            .initialize_captured_declaration_high_water(tidepool_repr::Generation(99))
            .unwrap();
        let child_owner =
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/child").unwrap(), 1)
                .unwrap();
        let mut child = PersistentSession::new(Some(child), 1024);
        let child_public = child.mint_isolated_scope();
        child
            .initialize_durable_public_scope(child_owner, child_public)
            .unwrap();
        assert_eq!(
            exomonad_actor::RootStartupManifestPin::capture_for_actor(store.path(), &owner)
                .unwrap(),
            before
        );
        assert!(root.validate_recovered_public_owner(&owner).unwrap());
    }

    #[test]
    fn existing_database_with_only_a_child_domain_has_no_recovered_root() {
        let run = tempfile::tempdir().unwrap();
        let lease = Arc::new(
            HostIncarnationLease::claim(
                &tidepool_atomic_write::DirectoryAnchor::open_existing(run.path()).unwrap(),
            )
            .unwrap(),
        );
        let store = Arc::new(RunRecoveryStore::create(&store_path(run.path())).unwrap());
        let id = tidepool_runtime::session::fresh_session_id();
        let child_root = run
            .path()
            .join("haskell-session-children")
            .join(id.0.to_string());
        let mut child = SessionLib::open(id, &child_root, ModuleEnv::standalone_default()).unwrap();
        tidepool_testing::with_settlement(|settlement| {
            attach_child(
                &mut child,
                run.path(),
                Arc::clone(&store),
                Arc::clone(&lease),
                settlement,
            )
        })
        .unwrap();
        assert!(store.read_domain(RecoveryDomainId::Root).unwrap().is_none());
        drop(child);
        drop(store);
        let mut root = library(run.path());
        tidepool_testing::with_settlement(|settlement| {
            attach(&mut root, run.path(), lease, settlement)
        })
        .unwrap();
        assert!(!root.has_recovered_domain().unwrap());
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
        assert!(!store_path(unrelated.path()).exists());
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
        assert!(!store_path(run.path()).exists());
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
        assert!(matches!(
            first
                .initialize_durable_public_scope(first_owner.clone(), public)
                .unwrap(),
            PublicManifestCommit::Durable,
        ));
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
