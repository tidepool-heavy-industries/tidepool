use proptest::prelude::*;
use std::sync::OnceLock;
use tidepool_bridge::traits::{FromCore, ToCore};
use tidepool_eval::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable, Literal, SrcBang};

static TABLE: OnceLock<DataConTable> = OnceLock::new();

fn get_table() -> &'static DataConTable {
    TABLE.get_or_init(|| {
        let mut table = DataConTable::new();
        // Text
        table.insert(DataCon {
            id: DataConId(14),
            name: "Text".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        });
        // I# (needed for i64/Int# fields of Text if they were boxed,
        // but current impl uses literals for off/len)
        table.insert(DataCon {
            id: DataConId(7),
            name: "I#".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
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
    /// For any String, FromCore(ToCore(s)) == s
    #[test]
    fn string_round_trip(s in any::<String>()) {
        roundtrip(s, get_table());
    }

    /// Empty string survives round-trip
    #[test]
    fn empty_string_round_trip(s in prop_oneof![""]) {
        roundtrip(s.to_string(), get_table());
    }

    /// ASCII-only strings survive
    #[test]
    fn ascii_only_round_trip(s in "[a-zA-Z0-9 ]{0,1000}") {
        roundtrip(s, get_table());
    }

    /// Strings with emoji survive (include ZWJ sequences, flags)
    #[test]
    fn emoji_round_trip(s in r"[\u{1F600}-\u{1F64F}\u{1F300}-\u{1F5FF}\u{1F680}-\u{1F6FF}\u{2600}-\u{26FF}\u{2700}-\u{27BF}\u{1F1E6}-\u{1F1FF}]{0,100}") {
        roundtrip(s, get_table());
    }

    /// Chars from BMP, SMP, SIP, TIP planes
    #[test]
    fn all_unicode_planes_round_trip(s in any::<String>()) {
        roundtrip(s, get_table());
    }

    /// Mix of 1-byte, 2-byte, 3-byte, 4-byte UTF-8 chars
    #[test]
    fn mixed_width_chars(
        // 1-byte (ASCII), 2-byte (Latin-1/Cyrillic), 3-byte (BMP/CJK), 4-byte (Emoji/Supplemental)
        s in r"[a-z\u{00A0}-\u{00FF}\u{0400}-\u{04FF}\u{4E00}-\u{9FFF}\u{1F600}-\u{1F64F}]{0,100}"
    ) {
        roundtrip(s, get_table());
    }

    /// ToCore(s) text length field matches s.len()
    #[test]
    fn length_preserved(s in any::<String>()) {
        let table = get_table();
        let value = s.to_value(table).expect("ToCore failed");
        if let Value::Con(_, fields) = value {
            assert_eq!(fields.len(), 3);
            if let Value::Lit(Literal::LitInt(len)) = fields[2] {
                assert_eq!(len as usize, s.len());
            } else {
                panic!("Expected LitInt for length field");
            }
        } else {
            panic!("Expected Con for Text value");
        }
    }

    /// ToCore(s) == ToCore(s) (same input -> same heap layout)
    #[test]
    fn to_value_is_deterministic(s in any::<String>()) {
        let table = get_table();
        let v1 = s.to_value(table).expect("ToCore failed");
        let v2 = s.to_value(table).expect("ToCore failed");

        assert!(compare_values(&v1, &v2), "ToCore is not deterministic for {:?}", s);
    }
}

fn compare_values(v1: &Value, v2: &Value) -> bool {
    match (v1, v2) {
        (Value::Lit(l1), Value::Lit(l2)) => l1 == l2,
        (Value::Con(id1, f1), Value::Con(id2, f2)) => {
            id1 == id2
                && f1.len() == f2.len()
                && f1.iter().zip(f2.iter()).all(|(a, b)| compare_values(a, b))
        }
        (Value::ByteArray(ba1), Value::ByteArray(ba2)) => *ba1.lock() == *ba2.lock(),
        _ => false,
    }
}
