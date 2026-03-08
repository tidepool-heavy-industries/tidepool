use tidepool_bridge::{FromCore, ToCore, BridgeError};
use tidepool_eval::value::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable, SrcBang, Literal};

fn get_table() -> DataConTable {
    let mut table = DataConTable::new();
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
    // Triple (,,)
    table.insert(DataCon {
        id: DataConId(11),
        name: "(,,)".to_string(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
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
        name: "D#".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![SrcBang::NoSrcBang],
        qualified_name: None,
    });
    table
}

#[test]
fn test_type_mismatch_int_to_string() {
    let table = get_table();
    let val = Value::Lit(Literal::LitInt(42));
    let res = String::from_value(&val, &table);
    assert!(matches!(res, Err(BridgeError::TypeMismatch { .. })));
}

#[test]
fn test_type_mismatch_int_to_bool() {
    let table = get_table();
    let val = Value::Lit(Literal::LitInt(1));
    let res = bool::from_value(&val, &table);
    assert!(matches!(res, Err(BridgeError::TypeMismatch { .. })));
}

#[test]
fn test_arity_mismatch_tuple() {
    let table = get_table();
    let triple_id = table.get_by_name("(,,)").unwrap();
    // Try to deserialize a 3-field Con as a 2-field tuple
    let val = Value::Con(triple_id, vec![
        Value::Lit(Literal::LitInt(1)),
        Value::Lit(Literal::LitInt(2)),
        Value::Lit(Literal::LitInt(3)),
    ]);
    let res = <(i64, i64)>::from_value(&val, &table);
    assert!(matches!(res, Err(BridgeError::UnknownDataCon(_)))); 
    // Wait, the impl of (A, B) checks if id == pair_id. 
    // If it's triple_id, it returns UnknownDataCon (or we could argue it's a type mismatch).
    // Let's check what the code does.
}

#[test]
fn test_nan_roundtrip() {
    let table = get_table();
    let val = f64::NAN;
    let value = val.to_value(&table).expect("ToCore failed");
    let back = f64::from_value(&value, &table).expect("FromCore failed");
    assert!(back.is_nan());
    assert_eq!(val.to_bits(), back.to_bits());
}

#[test]
fn test_edge_i64_min() {
    let table = get_table();
    let val = i64::MIN;
    let value = val.to_value(&table).unwrap();
    let back = i64::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_edge_i64_max() {
    let table = get_table();
    let val = i64::MAX;
    let value = val.to_value(&table).unwrap();
    let back = i64::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_edge_f64_inf() {
    let table = get_table();
    let val = f64::INFINITY;
    let value = val.to_value(&table).unwrap();
    let back = f64::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_edge_f64_neg_inf() {
    let table = get_table();
    let val = f64::NEG_INFINITY;
    let value = val.to_value(&table).unwrap();
    let back = f64::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_edge_empty_string() {
    let table = get_table();
    let val = "".to_string();
    // We need "Text" in table for String to_value
    let mut table = table;
    table.insert(DataCon {
        id: DataConId(14),
        name: "Text".to_string(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
        qualified_name: None,
    });
    let value = val.to_value(&table).unwrap();
    let back = String::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_edge_empty_vec() {
    let table = get_table();
    let val: Vec<i64> = vec![];
    let value = val.to_value(&table).unwrap();
    let back = Vec::<i64>::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}
