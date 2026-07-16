use tidepool_bridge::{BridgeError, FromCore, ToCore};
use tidepool_eval::value::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable, Literal, SrcBang};

fn get_table() -> DataConTable {
    // standard_datacon_table() covers False/True/(,)/[]/:/I#/D#/... (and more
    // that this suite doesn't need); append the 3-tuple it lacks, with a
    // fresh id, for test_arity_mismatch_tuple.
    let mut table = tidepool_testing::gen::standard_datacon_table();
    table.insert(DataCon {
        id: DataConId(100),
        name: "(,,)".to_string(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![SrcBang::NoSrcBang, SrcBang::NoSrcBang, SrcBang::NoSrcBang],
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
    let val = Value::Con(
        triple_id,
        vec![
            Value::Lit(Literal::LitInt(1)),
            Value::Lit(Literal::LitInt(2)),
            Value::Lit(Literal::LitInt(3)),
        ],
    );
    let res = <(i64, i64)>::from_value(&val, &table);
    assert!(matches!(res, Err(BridgeError::TypeMismatch { .. })));
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
    // standard_datacon_table() already carries "Text" for String to_value.
    let table = get_table();
    let val = "".to_string();
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

#[test]
fn test_phantom_data_missing_unit() {
    use std::marker::PhantomData;
    let table = get_table(); // has False, True, but no "()"
    let val: PhantomData<()> = PhantomData;
    let res = val.to_value(&table);
    assert!(matches!(res, Err(BridgeError::UnknownDataConName(ref s)) if s == "()"));
}

#[test]
fn test_text_decode_wrong_shape() {
    let table = get_table();
    let text_id = table.get_by_name("Text").unwrap();
    let false_id = table.get_by_name("False").unwrap();
    let val = Value::Con(
        text_id,
        vec![
            Value::Con(false_id, vec![]), // the wrong shape
            Value::Lit(Literal::LitInt(0)),
            Value::Lit(Literal::LitInt(0)),
        ],
    );
    let res = String::from_value(&val, &table);
    match res {
        Err(BridgeError::TypeMismatch { expected, got }) => {
            assert!(expected.contains("ByteArray"));
            assert_eq!(got, "Con(False)");
        }
        _ => panic!("Expected TypeMismatch with Con(False), got {:?}", res),
    }
}
