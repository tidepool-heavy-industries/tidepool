// Pins RootCustody's use-after-move guarantee: `into_handle` consumes `self`
// by value, so a second call on the same binding must be a compile error, not
// a runtime double-custody bug. `RootCustody::new` is `pub(crate)`, so this
// takes the token as a function parameter instead of minting one directly —
// that keeps the fixture reachable through the token's PUBLIC API and lands
// on the intended use-after-move diagnostic rather than a privacy error.
fn double_consume(custody: tidepool_runtime::session::RootCustody) {
    let _delivered = custody.into_handle();
    let _mounted = custody.into_handle();
}

fn main() {}
