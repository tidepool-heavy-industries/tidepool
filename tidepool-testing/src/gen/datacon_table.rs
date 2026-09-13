use tidepool_repr::datacon::SrcBang;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::AltCon;
use tidepool_repr::{CoreExpr, DataCon, DataConId, DataConTable};

/// Walk the tree to find all `DataConId`s and their maximum observed arities.
pub fn build_table_for_expr(expr: &CoreExpr) -> DataConTable {
    let mut table = standard_datacon_table();
    let mut seen = std::collections::HashMap::new();

    for node in &expr.nodes {
        match node {
            CoreFrame::Con { tag, fields } => {
                let arity = fields.len() as u32;
                let entry = seen.entry(*tag).or_insert(0);
                if arity > *entry {
                    *entry = arity;
                }
            }
            CoreFrame::Case { alts, .. } => {
                for alt in alts {
                    if let AltCon::DataAlt(tag) = alt.con {
                        let arity = alt.binders.len() as u32;
                        let entry = seen.entry(tag).or_insert(0);
                        if arity > *entry {
                            *entry = arity;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for (id, arity) in seen {
        if table.get(id).is_none() {
            table.insert(DataCon {
                id,
                name: format!("C{}", id.0),
                tag: (id.0 % 100) as u32 + 1,
                rep_arity: arity,
                field_bangs: vec![],
                qualified_name: None,
                type_name: String::new(),
            });
        }
    }

    table
}

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
