use tidepool_runtime::session::{PendingPreparedInstall, ResidentSession};

fn cross_candidates(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    a: PendingPreparedInstall,
    b: PendingPreparedInstall,
) {
    let compiled_b = b.compile_off_checkout().unwrap();
    session.revalidate_and_run_prepared(a, compiled_b).unwrap();
}

fn main() {}
