fn successive_leases(session: &mut tidepool_runtime::session::PersistentSession) {
    let first = session.lease_machine();
    drop(first);
    let second = session.lease_machine();
    drop(second);
}

fn main() {}
