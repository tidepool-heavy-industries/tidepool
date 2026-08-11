use proptest::prelude::*;
use std::sync::OnceLock;
use tidepool_bridge::traits::{FromCore, ToCore};
use tidepool_repr::{DataCon, DataConId, DataConTable, SrcBang};

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
            qualified_name: None,
            type_name: String::new(),
        });
        // I# (needed for i64/Int# fields of Text if they were boxed,
        // but current impl uses literals for off/len)
        table.insert(DataCon {
            id: DataConId(7),
            name: "I#".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: None,
            type_name: String::new(),
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

    /// Strings with emoji survive (include ZWJ sequences, flags)
    #[test]
    fn emoji_round_trip(s in r"[\u{1F600}-\u{1F64F}\u{1F300}-\u{1F5FF}\u{1F680}-\u{1F6FF}\u{2600}-\u{26FF}\u{2700}-\u{27BF}\u{1F1E6}-\u{1F1FF}]{0,100}") {
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
}
