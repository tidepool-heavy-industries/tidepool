use std::sync::Arc;
use tidepool_runtime::session::{
    FrozenWorldExecution, PersistentSession, PrivateExecutionAdmission, SessionError,
};

fn freeze(
    session: &mut PersistentSession,
    admission: &PrivateExecutionAdmission,
) -> Result<Arc<FrozenWorldExecution>, SessionError> {
    session.freeze_private_execution(admission)
}

fn main() {}
