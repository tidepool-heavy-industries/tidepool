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
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(1),
        name: "Just".to_string(),
        tag: 2,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    // Bool
    table.insert(DataCon {
        id: DataConId(2),
        name: "False".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(3),
        name: "True".to_string(),
        tag: 2,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    // Pair (,)
    table.insert(DataCon {
        id: DataConId(4),
        name: "(,)".to_string(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    // List [] and :
    table.insert(DataCon {
        id: DataConId(5),
        name: "[]".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(6),
        name: ":".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    // Boxing
    table.insert(DataCon {
        id: DataConId(7),
        name: "I#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(8),
        name: "W#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(9),
        name: "D#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(10),
        name: "C#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    // Text (Data.Text.Internal.Text)
    table.insert(DataCon {
        id: DataConId(11),
        name: "Text".to_string(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}

/// A `DataConTable` holding the freer-simple continuation constructors
/// (`Val`, `E`, `Leaf`, `Node`, `Union`) `EffectMachine` needs to walk an
/// `Eff` expression. Shared by `tidepool-effect`'s unit tests
/// (`machine.rs`) and its `proptest_effect_machine.rs` integration suite.
pub fn freer_effect_test_table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: DataConId(1),
        name: "Val".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(2),
        name: "E".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(3),
        name: "Leaf".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(4),
        name: "Node".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: DataConId(5),
        name: "Union".to_string(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}
