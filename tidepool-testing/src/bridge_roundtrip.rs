//! Shared `ToHaskell`/`FromHaskell` roundtrip assertion for `tidepool-bridge`'s test
//! suites (its own unit tests, `tests/roundtrip.rs`, `tests/proptest_text.rs`).

use tidepool_bridge::traits::{FromHaskell, ToHaskell};
use tidepool_repr::DataConTable;

/// Encode `val` via `ToHaskell`, decode it back via `FromHaskell`, and assert the
/// result equals the original.
pub fn roundtrip<T: FromHaskell + ToHaskell + PartialEq + std::fmt::Debug>(
    val: T,
    table: &DataConTable,
) {
    #[allow(clippy::expect_used, reason = "ToHaskell failed")]
    let value = val.to_value(table).expect("ToHaskell failed");
    #[allow(clippy::expect_used, reason = "FromHaskell failed")]
    let back = T::from_value(&value, table).expect("FromHaskell failed");
    assert_eq!(val, back, "Roundtrip failed for {:?}", val);
}
