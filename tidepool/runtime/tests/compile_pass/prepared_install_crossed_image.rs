use tidepool_runtime::session::{PendingPreparedInstall, ResidentSession};

fn install(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    pending: PendingPreparedInstall,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) {
    let ready = pending.compile_off_checkout().unwrap();
    session
        .revalidate_and_run_prepared(ready, settlement)
        .unwrap();
}

fn main() {}
