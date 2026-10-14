use tidepool_codegen::suspension::RealmId;
use tidepool_runtime::session::{ResidentHole, ResidentSession};

fn run(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    hole: &ResidentHole,
    site: i64,
    realm: RealmId,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) {
    let entry = session
        .capture_request_receiver_entry(hole, site, realm)
        .unwrap();
    session
        .run_request_receiver_entry(entry, settlement)
        .unwrap();
}

fn main() {}
