use tidepool_runtime::session::PendingPreparedInstall;

fn compile_once(pending: PendingPreparedInstall) {
    let _ready = pending.compile_off_checkout().unwrap();
}

fn main() {}
