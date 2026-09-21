use tidepool_bridge::{FromHaskell, ToHaskell};
use tidepool_repr::DataConTable;

pub fn roundtrip<T: FromHaskell + ToHaskell + PartialEq + std::fmt::Debug>(
    value: T,
    table: &DataConTable,
) {
    let encoded = value.to_value(table).expect("ToHaskell failed");
    let decoded = T::from_value(&encoded, table).expect("FromHaskell failed");
    assert_eq!(value, decoded, "roundtrip failed");
}
