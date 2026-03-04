use tidepool_repr::datacon::SrcBang;
use tidepool_repr::{DataCon, DataConId, DataConTable};

/// Returns a standard DataConTable with common types like Maybe, Bool, and Pair.
pub fn standard_datacon_table() -> DataConTable {
    let mut table = DataConTable::new();
    // Maybe
    table.insert(DataCon {
        id: DataConId(0),
        name: "Nothing".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: DataConId(1),
        name: "Just".to_string(),
        tag: 2,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
    });
    // Bool
    table.insert(DataCon {
        id: DataConId(2),
        name: "False".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: DataConId(3),
        name: "True".to_string(),
        tag: 2,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    // Pair (,)
    table.insert(DataCon {
        id: DataConId(4),
        name: "(,)".to_string(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        qualified_name: None,
    });
    // List [] and :
    table.insert(DataCon {
        id: DataConId(5),
        name: "[]".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: DataConId(6),
        name: ":".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        qualified_name: None,
    });
    // Boxing
    table.insert(DataCon {
        id: DataConId(7),
        name: "I#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: DataConId(8),
        name: "W#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: DataConId(9),
        name: "D#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: DataConId(10),
        name: "C#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
    });
    // Text (Data.Text.Internal.Text)
    table.insert(DataCon {
        id: DataConId(11),
        name: "Text".to_string(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        qualified_name: None,
    });
    table
}
