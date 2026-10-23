use std::sync::Arc;
use tidepool_runtime::session::PersistentSession;
use tidepool_toolchain::declaration_join::ExactDeclarationContext;

fn freeze(session: &mut PersistentSession, recovered: Arc<ExactDeclarationContext>) {
    let _ = session.freeze_private_execution(&recovered);
}

fn main() {}
