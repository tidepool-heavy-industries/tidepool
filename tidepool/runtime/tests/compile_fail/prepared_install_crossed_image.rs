use tidepool_runtime::session::{PendingPreparedInstall, ResidentSession};

fn cross_candidates(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    a: PendingPreparedInstall,
    b: PendingPreparedInstall,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) {
    let compiled_b = b.compile_off_checkout().unwrap();
    session
        .revalidate_and_run_prepared(a, compiled_b, settlement)
        .unwrap();
}

fn main() {}
