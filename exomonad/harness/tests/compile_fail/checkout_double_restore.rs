// Pins Checkout's use-after-move guarantee: `restore_suspended` consumes
// `self` by value, so settling the SAME checkout twice must be a compile
// error, not a runtime double-settle bug. `Checkout` is generic over the
// machine handle `M`; `()` is a concrete stand-in that exercises the same
// non-generic consumption shape a real machine handle would.
fn double_restore(checkout: tidepool_harness::registry::Checkout<'_, ()>) {
    checkout.restore_suspended(Vec::new());
    checkout.restore_suspended(Vec::new());
}

fn main() {}
