// Pins Checkout's use-after-move guarantee: `restore_idle` consumes `self` by
// value, so settling the SAME checkout twice must be a compile error, not a
// runtime double-settle bug. `Checkout` is generic over the machine handle
// `M`; `()` is a concrete stand-in that exercises the same non-generic
// consumption shape a real machine handle would.
fn double_restore(checkout: tidepool_harness::registry::Checkout<'_, ()>) {
    checkout.restore_idle();
    checkout.restore_idle();
}

fn main() {}
