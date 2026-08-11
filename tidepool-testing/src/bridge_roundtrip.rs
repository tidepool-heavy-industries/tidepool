//! Shared `ToCore`/`FromCore` roundtrip assertion for `tidepool-bridge`'s test
//! suites (its own unit tests, `tests/roundtrip.rs`, `tests/proptest_text.rs`).

use tidepool_bridge::traits::{FromCore, ToCore};
use tidepool_repr::DataConTable;

/// Encode `val` via `ToCore`, decode it back via `FromCore`, and assert the
/// result equals the original.
pub fn roundtrip<T: FromCore + ToCore + PartialEq + std::fmt::Debug>(val: T, table: &DataConTable) {
    let value = val.to_value(table).expect("ToCore failed");
    let back = T::from_value(&value, table).expect("FromCore failed");
    assert_eq!(val, back, "Roundtrip failed for {:?}", val);
}
