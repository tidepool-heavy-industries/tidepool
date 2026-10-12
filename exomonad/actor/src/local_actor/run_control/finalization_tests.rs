//! Run finalization exercises the actual journal writer and exclusive lease.
use super::*;
use std::fs::{File, OpenOptions};
use std::path::Path;
use tidepool_atomic_write::DirectoryAnchor;
use tidepool_runtime::session::RecoveryRunAuthority;

struct LockedRun {
    directory: Arc<tempfile::TempDir>,
    _lock: File,
}
impl RecoveryRunAuthority for LockedRun {
    fn owns_run(&self, path: &Path) -> std::io::Result<bool> {
        Ok(path.canonicalize()? == self.directory.path().canonicalize()?)
    }
}
impl crate::RecoveryBootRegistration for LockedRun {
    fn into_run(self: Box<Self>) -> Arc<dyn RecoveryRunAuthority> {
        Arc::new(*self)
    }
}

async fn boot(
    directory: Arc<tempfile::TempDir>,
) -> (
    Arc<crate::ActorRecoveryJournal>,
    crate::JournalBootRegistration,
) {
    let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let (journal, registration) = crate::ActorRecoveryJournal::open_for_boot(
        &anchor,
        "actors.jsonl",
        false,
        Box::new(LockedRun {
            directory,
            _lock: lock,
        }),
    )
    .await
    .unwrap();
    (journal, registration)
}

fn competitor(directory: &tempfile::TempDir) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("run-owner.lock"))
        .unwrap()
}
fn assert_excluded(file: &File) {
    assert!(matches!(
        file.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
}
async fn stopped(mut status: tokio::sync::watch::Receiver<crate::JournalOwnerStatus>) {
    while !matches!(*status.borrow(), crate::JournalOwnerStatus::Stopped { .. }) {
        status.changed().await.unwrap();
    }
}

#[tokio::test]
async fn confirmed_finish_releases_boot_lease_after_journal_and_lifecycle_exit() {
    for abandoned in [false, true] {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let (journal, registration) = boot(directory.clone()).await;
        let status = journal.owner_status();
        let (control, task) = RunControlClient::start().await.unwrap();
        control
            .register_recovery_boot(journal.clone(), registration)
            .await
            .unwrap();
        if abandoned {
            drop(control.reserve(crate::Incarnation::FIRST).await.unwrap());
        }
        let competing = competitor(&directory);
        assert_excluded(&competing);
        control.seal().await.unwrap();
        assert!(matches!(
            control.settle_preparations(Duration::from_secs(1)).await,
            crate::CleanupComponentOutcome::Confirmed
        ));
        assert!(matches!(
            control.shutdown_run_resources().await,
            crate::CleanupComponentOutcome::Confirmed
        ));
        control.finish_run().await.unwrap();
        task.await.unwrap();
        // A live external journal handle still owns its writer's lease.
        assert_excluded(&competing);
        drop(journal);
        stopped(status).await;
        competing.try_lock().unwrap();
    }
}

#[tokio::test]
async fn uncertain_preparation_refuses_finish_and_keeps_actual_boot_lease() {
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let (journal, registration) = boot(directory.clone()).await;
    let status = journal.owner_status();
    let (control, task) = RunControlClient::start().await.unwrap();
    control
        .register_recovery_boot(journal.clone(), registration)
        .await
        .unwrap();
    let reservation = control.reserve(crate::Incarnation::FIRST).await.unwrap();
    let actor = reservation.identity();
    let path = directory.path().join("actors.jsonl");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let writer = journal.clone();
    let operation: RootOperation = Box::new(move |_ticket| {
        Box::pin(async move {
            let descriptor = crate::ActorDescriptor::new(
                "reserved",
                crate::ActorPlacement {
                    session: tidepool_repr::SessionId(1),
                    lexical_scope: tidepool_codegen::scope::ScopeId(1),
                    resource_scope: tidepool_codegen::suspension::RealmId(1),
                },
            );
            let error = writer.admit(actor, &descriptor, &[]).await.unwrap_err();
            let uncertainty = crate::CleanupComponentOutcome::Unconfirmed(error.to_string());
            (
                Err(Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
                uncertainty,
            )
        })
    });
    assert!(control.prepare_root(operation).await.unwrap().is_err());
    drop(reservation);
    control.seal().await.unwrap();
    assert!(matches!(
        control.settle_preparations(Duration::from_secs(1)).await,
        crate::CleanupComponentOutcome::Unconfirmed(_)
    ));
    assert!(matches!(
        control.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    let competing = competitor(&directory);
    drop(journal);
    assert!(control.finish_run().await.is_err());
    assert_excluded(&competing);
    // The isolated failed host is terminated explicitly; this is not a
    // successful finalization receipt and does not discharge its uncertainty.
    task.abort();
    let _ = task.await;
    stopped(status).await;
}

#[tokio::test]
async fn configured_resident_finish_clears_projection_and_releases_actual_run_lease() {
    use tidepool_runtime::session::{ModuleEnv, ResidentSession, SessionLib};
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let (journal, registration) = boot(directory.clone()).await;
    let status = journal.owner_status();
    let session_root = tempfile::tempdir().unwrap();
    let session_id = tidepool_runtime::session::fresh_session_id();
    let library = SessionLib::open(
        session_id,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let machine = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    let (forest, _deployments) = crate::ResidentForest::new(
        crate::ActorWorkbenchSource::new("", Vec::new()),
        session_id,
        machine,
        None,
        crate::Incarnation::FIRST,
    )
    .await
    .unwrap();
    let forest = forest.with_recovery_journal(journal.clone());
    forest.register_recovery_boot(registration).await.unwrap();
    let actor = forest
        .new_workbench("lease-owner".into(), crate::ActorCapabilities::default())
        .await
        .unwrap();
    let lifecycle = actor.lifecycle().clone();
    let resident = lifecycle
        .resident_snapshot()
        .expect("production resident configured");
    assert_eq!(resident.context.actor, actor.identity());
    drop(resident);
    assert!(journal
        .records()
        .await
        .unwrap()
        .iter()
        .any(|record| { record.admission.actor == actor.identity() }));
    let competing = competitor(&directory);
    assert_excluded(&competing);
    let outcomes = forest.shutdown().await;
    assert!(
        outcomes.iter().all(crate::ForestRootShutdown::is_confirmed),
        "production resident cleanup must confirm: {outcomes:?}"
    );
    assert!(
        lifecycle.resident_snapshot().is_none(),
        "finished owner clears its resident projection"
    );
    assert_excluded(&competing);
    // Keep the actor and lifecycle observations alive: neither may retain the
    // resident journal after the confirmed production finalization boundary.
    drop(forest);
    drop(journal);
    tokio::time::timeout(Duration::from_secs(5), stopped(status))
        .await
        .unwrap();
    competing.try_lock().unwrap();
    drop(actor);
    drop(lifecycle);
}
