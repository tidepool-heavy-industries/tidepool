use tidepool_runtime::session::PendingPreparedInstall;

fn compile_twice(pending: PendingPreparedInstall) {
    let _first = pending.compile_off_checkout().unwrap();
    let _second = pending.compile_off_checkout().unwrap();
}

fn main() {}
