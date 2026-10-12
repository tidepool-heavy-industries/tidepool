use super::*;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

pub(super) type Forest = ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>;

/// Own the real empty resident forest and its host-side resources. Tests choose
/// their actor behaviors and explicitly shut down the forest before returning.
pub(super) struct TestForest {
    pub(super) forest: Arc<Forest>,
    // Keep publication available for the entire test, even when it does not
    // inspect deployments. Dropping this receiver changes retirement admission.
    _deployments: mpsc::Receiver<LocalResidentDeployment>,
    _session_root: tempfile::TempDir,
}

impl TestForest {
    pub(super) async fn new() -> Self {
        let session_root = tempfile::tempdir().expect("session root");
        let session_id = tidepool_runtime::session::fresh_session_id();
        let library = SessionLib::open(
            session_id,
            session_root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("ephemeral declaration owner");
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        let (forest, deployments) = Forest::new(
            ActorWorkbenchSource::new("", Vec::new()),
            session_id,
            machine,
            None,
            crate::Incarnation::FIRST,
        )
        .await
        .expect("actual resident forest owner");
        Self {
            forest: Arc::new(forest),
            _deployments: deployments,
            _session_root: session_root,
        }
    }
}
