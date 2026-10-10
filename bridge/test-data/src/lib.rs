//! Small, representation-only fixtures shared by low-level tests.

use tidepool_repr::datacon::SrcBang;
use tidepool_repr::{DataCon, DataConId, DataConTable};

/// Returns a standard DataConTable with common types like Maybe, Bool, and Pair.
pub fn standard_datacon_table() -> DataConTable {
    let mut table = DataConTable::new();
    // Maybe
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Maybe".into(),
                namespace: "constructor".into(),
                occurrence: "Nothing".into(),
                record_parent: None,
            },
            id: DataConId(0),
            name: "Nothing".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Maybe.Nothing".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Maybe".into(),
                namespace: "constructor".into(),
                occurrence: "Just".into(),
                record_parent: None,
            },
            id: DataConId(1),
            name: "Just".to_string(),
            tag: 2,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: Some("GHC.Maybe.Just".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    // Bool
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: "False".into(),
                record_parent: None,
            },
            id: DataConId(2),
            name: "False".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Types.False".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: "True".into(),
                record_parent: None,
            },
            id: DataConId(3),
            name: "True".to_string(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Types.True".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    // Pair (,)
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Tuple".into(),
                namespace: "constructor".into(),
                occurrence: "(,)".into(),
                record_parent: None,
            },
            id: DataConId(4),
            name: "(,)".to_string(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
            qualified_name: Some("GHC.Tuple.(,)".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    // List [] and :
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: "[]".into(),
                record_parent: None,
            },
            id: DataConId(5),
            name: "[]".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Types.[]".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: ":".into(),
                record_parent: None,
            },
            id: DataConId(6),
            name: ":".to_string(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang],
            qualified_name: Some("GHC.Types.:".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    // Boxing
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: "I#".into(),
                record_parent: None,
            },
            id: DataConId(7),
            name: "I#".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: Some("GHC.Types.I#".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: "W#".into(),
                record_parent: None,
            },
            id: DataConId(8),
            name: "W#".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: Some("GHC.Types.W#".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: "D#".into(),
                record_parent: None,
            },
            id: DataConId(9),
            name: "D#".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: Some("GHC.Types.D#".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "GHC.Types".into(),
                namespace: "constructor".into(),
                occurrence: "C#".into(),
                record_parent: None,
            },
            id: DataConId(10),
            name: "C#".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: Some("GHC.Types.C#".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    // Text (the extractor normalizes Data.Text.Internal to Data.Text)
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Data.Text".into(),
                namespace: "constructor".into(),
                occurrence: "Text".into(),
                record_parent: None,
            },
            id: DataConId(11),
            name: "Text".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
            qualified_name: Some("Data.Text.Text".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    // Either
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Data.Either".into(),
                namespace: "constructor".into(),
                occurrence: "Left".into(),
                record_parent: None,
            },
            id: DataConId(12),
            name: "Left".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: Some("Data.Either.Left".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
        .insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "fixture".into(),
                module: "Data.Either".into(),
                namespace: "constructor".into(),
                occurrence: "Right".into(),
                record_parent: None,
            },
            id: DataConId(13),
            name: "Right".to_string(),
            tag: 2,
            rep_arity: 1,
            field_bangs: vec![SrcBang::NoSrcBang],
            qualified_name: Some("Data.Either.Right".into()),
            type_name: String::new(),
        })
        .expect("valid fixture metadata");
    table
}

/// Typed codecs for structural prepared-schema fixtures.
pub mod prepared_encode;

/// Declared immutable fixture resources read at test runtime.
pub mod prepared_resources;

/// Representation fixtures with no compiler provenance.
pub mod prepared;
