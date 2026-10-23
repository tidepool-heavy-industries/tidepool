use std::sync::Arc;
use tidepool_toolchain::declaration_join::{CompletedOriginalExecution, ExactDeclarationContext};
fn consume(_: CompletedOriginalExecution<'_>) {}
fn cannot_complete(context: Arc<ExactDeclarationContext>) {
    consume(context);
}
fn main() {}
