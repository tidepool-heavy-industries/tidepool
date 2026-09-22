// Pins MachineLease's exclusivity guarantee: `lease_machine` takes `&mut
// self`, so two outstanding leases on the same session must be a compile
// error (a double mutable borrow), not a runtime race over the machine slot.
fn double_lease(session: &mut tidepool_runtime::session::PersistentSession) {
    let lease1 = session.lease_machine();
    let lease2 = session.lease_machine();
    let _ = (lease1, lease2);
}

fn main() {}
