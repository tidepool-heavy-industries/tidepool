use tidepool_runtime::session::{PendingPreparedInstall, ResidentSession};

fn install(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    pending: PendingPreparedInstall,
) {
    let ready = pending.compile_off_checkout().unwrap();
    session.revalidate_and_run_prepared(ready).unwrap();
}

fn main() {}
