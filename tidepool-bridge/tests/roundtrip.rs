use proptest::prelude::*;
use std::sync::OnceLock;
use tidepool_bridge::traits::{FromCore, ToCore};
use tidepool_repr::{DataCon, DataConId, DataConTable, SrcBang};

static TABLE: OnceLock<DataConTable> = OnceLock::new();

fn get_table() -> &'static DataConTable {
    TABLE.get_or_init(|| {
        // standard_datacon_table() covers Nothing/Just/False/True/(,)/[]/:/
        // I#/W#/D#/C#/Text; append the constructors it lacks that these
        // proptests still need (3-tuple, Either) with fresh ids.
        let mut table = tidepool_testing::gen::standard_datacon_table();
        table.insert(DataCon {
            id: DataConId(100),
            name: "(,,)".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
            qualified_name: None,
        });
        table.insert(DataCon {
            id: DataConId(101),
            name: "Right".to_string(),
            tag: 2,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: None,
        });
        table.insert(DataCon {
            id: DataConId(102),
            name: "Left".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: None,
        });
        table
    })
}

fn roundtrip<T: FromCore + ToCore + PartialEq + std::fmt::Debug>(val: T, table: &DataConTable) {
    let value = val.to_value(table).expect("ToCore failed");
    let back = T::from_value(&value, table).expect("FromCore failed");
    assert_eq!(val, back, "Roundtrip failed for {:?}", val);
}

proptest! {
    #[test]
    fn prop_i64_roundtrip(val in any::<i64>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_f64_roundtrip(val in any::<f64>()) {
        if !val.is_nan() {
            roundtrip(val, get_table());
        } else {
            // For NaN, compare bits as NaN != NaN
            let table = get_table();
            let value = val.to_value(table).expect("ToCore failed");
            let back = f64::from_value(&value, table).expect("FromCore failed");
            assert_eq!(val.to_bits(), back.to_bits());
        }
    }

    #[test]
    fn prop_bool_roundtrip(val in any::<bool>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_string_roundtrip(val in any::<String>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_option_i64_roundtrip(val in any::<Option<i64>>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_vec_i64_roundtrip(val in any::<Vec<i64>>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_tuple_i64_i64_roundtrip(val in any::<(i64, i64)>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_u64_roundtrip(val in any::<u64>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_char_roundtrip(val in any::<char>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_tuple3_roundtrip(val in any::<(i64, bool, String)>()) {
        roundtrip(val, get_table());
    }

    #[test]
    fn prop_result_roundtrip(val in any::<Result<i64, String>>()) {
        roundtrip(val, get_table());
    }
}
