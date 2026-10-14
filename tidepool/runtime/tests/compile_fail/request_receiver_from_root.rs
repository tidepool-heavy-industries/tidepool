use tidepool_runtime::session::{ResidentSession, RootCustody};

fn run(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    root: RootCustody,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) {
    session
        .run_request_receiver_entry(root, settlement)
        .unwrap();
}

fn main() {}
