use proptest::prelude::*;
use std::sync::OnceLock;

use super::support::roundtrip;
use tidepool_bridge::traits::{FromHaskell, ToHaskell};
use tidepool_repr::{DataCon, DataConId, DataConTable, SrcBang};

static TABLE: OnceLock<DataConTable> = OnceLock::new();

fn get_table() -> &'static DataConTable {
    TABLE.get_or_init(|| {
        // The shared table already owns Either; only the triple is additional.
        let mut table = tidepool_test_data::standard_datacon_table();
        table
            .insert_checked(DataCon {
                identity: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: "fixture".into(),
                    module: "GHC.Tuple".into(),
                    namespace: "constructor".into(),
                    occurrence: "(,,)".into(),
                    record_parent: None,
                },
                id: DataConId(100),
                name: "(,,)".to_string(),
                tag: 1,
                rep_arity: 3,
                field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
                qualified_name: Some("GHC.Tuple.(,,)".into()),
                type_name: String::new(),
            })
            .expect("valid fixture metadata");
        table
    })
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
            let value = val.to_value(table).expect("ToHaskell failed");
            let back = f64::from_value(&value, table).expect("FromHaskell failed");
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
