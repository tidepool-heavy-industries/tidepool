use std::sync::Arc;
use tidepool_runtime::session::{
    FrozenWorldExecution, PersistentSession, PrivateExecutionAdmission, SessionError,
};

fn freeze(session: &mut PersistentSession, admission: &PrivateExecutionAdmission) {
    let _ = session.freeze_execution_intent(admission, Vec::new(), Vec::new());
}

fn main() {}
