use tidepool_runtime::session::{BoundBinder, PendingDisplayInstall, ResidentSession};

fn cross_candidates(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    a: PendingDisplayInstall,
    b: PendingDisplayInstall,
    page: &BoundBinder,
    alias: &BoundBinder,
) {
    let compiled_b = b.compile_off_checkout().unwrap();
    session
        .revalidate_and_run_display_bundle(a, compiled_b, page, alias)
        .unwrap();
}

fn main() {}
