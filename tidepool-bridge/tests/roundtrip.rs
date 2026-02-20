use proptest::prelude::*;
use tidepool_bridge::traits::{FromCore, ToCore};
use tidepool_repr::{DataCon, DataConId, DataConTable, SrcBang};

fn test_table() -> DataConTable {
    let mut table = DataConTable::new();
    // Maybe
    table.insert(DataCon {
        id: DataConId(0),
        name: "Nothing".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
    });
    table.insert(DataCon {
        id: DataConId(1),
        name: "Just".to_string(),
        tag: 2,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
    });
    // Bool
    table.insert(DataCon {
        id: DataConId(2),
        name: "False".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
    });
    table.insert(DataCon {
        id: DataConId(3),
        name: "True".to_string(),
        tag: 2,
        rep_arity: 0,
        field_bangs: vec![],
    });
    // Pair (,)
    table.insert(DataCon {
        id: DataConId(4),
        name: "(,)".to_string(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
    });
    // List [] and :
    table.insert(DataCon {
        id: DataConId(5),
        name: "[]".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
    });
    table.insert(DataCon {
        id: DataConId(6),
        name: ":".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
    });
    // Boxing
    table.insert(DataCon {
        id: DataConId(7),
        name: "I#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
    });
    table.insert(DataCon {
        id: DataConId(8),
        name: "D#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
    });
    table.insert(DataCon {
        id: DataConId(9),
        name: "W#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
    });
    table.insert(DataCon {
        id: DataConId(10),
        name: "C#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
    });
    table.insert(DataCon {
        id: DataConId(11),
        name: "(,,)".to_string(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
    });
    table.insert(DataCon {
        id: DataConId(12),
        name: "Right".to_string(),
        tag: 2,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
    });
    table.insert(DataCon {
        id: DataConId(13),
        name: "Left".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
    });
    table
}

fn roundtrip<T: FromCore + ToCore + PartialEq + std::fmt::Debug>(val: T, table: &DataConTable) {
    let value = val.to_value(table).expect("ToCore failed");
    let back = T::from_value(&value, table).expect("FromCore failed");
    assert_eq!(val, back, "Roundtrip failed for {:?}", val);
}

proptest! {
    #[test]
    fn prop_i64_roundtrip(val in any::<i64>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_f64_roundtrip(val in any::<f64>()) {
        if !val.is_nan() {
            roundtrip(val, &test_table());
        } else {
            // For NaN, compare bits as NaN != NaN
            let table = test_table();
            let value = val.to_value(&table).expect("ToCore failed");
            let back = f64::from_value(&value, &table).expect("FromCore failed");
            assert_eq!(val.to_bits(), back.to_bits());
        }
    }

    #[test]
    fn prop_bool_roundtrip(val in any::<bool>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_string_roundtrip(val in any::<String>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_option_i64_roundtrip(val in any::<Option<i64>>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_vec_i64_roundtrip(val in any::<Vec<i64>>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_tuple_i64_i64_roundtrip(val in any::<(i64, i64)>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_u64_roundtrip(val in any::<u64>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_char_roundtrip(val in any::<char>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_tuple3_roundtrip(val in any::<(i64, bool, String)>()) {
        roundtrip(val, &test_table());
    }

    #[test]
    fn prop_result_roundtrip(val in any::<Result<i64, String>>()) {
        roundtrip(val, &test_table());
    }
}
