use tidepool_runtime::session::{PendingDisplayInstall, PendingPreparedInstall};

fn compile_twice(pending: PendingPreparedInstall) {
    let _first = pending.compile_off_checkout().unwrap();
    let _second = pending.compile_off_checkout().unwrap();
}

fn compile_display_twice(pending: PendingDisplayInstall) {
    let _first = pending.compile_off_checkout().unwrap();
    let _second = pending.compile_off_checkout().unwrap();
}

fn main() {}
