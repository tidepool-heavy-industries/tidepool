//! Fixed bridge schemas resolve compiler-published constructor identities.
//! Same-shaped user constructors must neither block a canonical constructor
//! nor stand in for one when the canonical constructor is absent.

use tidepool_bridge::{BridgeError, FromHaskell, HaskellValue, ToHaskell};
use tidepool_repr::{DataCon, DataConId, DataConTable, Literal};

fn insert(table: &mut DataConTable, id: u64, name: &str, arity: u32, qualified: &str) {
    table.insert(DataCon {
        id: DataConId(id),
        name: name.into(),
        tag: 1,
        rep_arity: arity,
        field_bangs: vec![],
        qualified_name: Some(qualified.into()),
        type_name: String::new(),
    });
}

#[test]
fn canonical_primitive_wins_over_same_shaped_impostor() {
    let mut table = DataConTable::new();
    insert(&mut table, 100, "I#", 1, "GHC.Types.I#");
    insert(&mut table, 200, "I#", 1, "UserDefined.I#");

    let encoded = 42i64
        .to_value(&table)
        .expect("the canonical I# remains authoritative");
    assert!(matches!(
        encoded,
        HaskellValue::Con(DataConId(100), ref fields)
            if matches!(fields.as_slice(), [HaskellValue::Lit(Literal::LitInt(42))])
    ));
    assert_eq!(i64::from_value(&encoded, &table).unwrap(), 42);

    let impostor = HaskellValue::Con(DataConId(200), vec![HaskellValue::Lit(Literal::LitInt(42))]);
    assert!(matches!(
        i64::from_value(&impostor, &table),
        Err(BridgeError::TypeMismatch { .. })
    ));
}

#[test]
fn impostor_only_primitive_table_cannot_encode_or_decode() {
    let mut table = DataConTable::new();
    insert(&mut table, 200, "I#", 1, "UserDefined.I#");

    let err = 42i64
        .to_value(&table)
        .expect_err("an impostor I# cannot satisfy the fixed Int schema");
    assert!(matches!(err, BridgeError::UnknownDataConName(ref name) if name == "I#"));

    let impostor = HaskellValue::Con(DataConId(200), vec![HaskellValue::Lit(Literal::LitInt(42))]);
    assert!(matches!(
        i64::from_value(&impostor, &table),
        Err(BridgeError::TypeMismatch { .. })
    ));
}

#[test]
fn canonical_name_with_wrong_arity_is_rejected() {
    let mut table = DataConTable::new();
    insert(&mut table, 100, "I#", 2, "GHC.Types.I#");
    let err = 42i64
        .to_value(&table)
        .expect_err("canonical identity does not override representation arity");
    assert!(matches!(err, BridgeError::UnknownDataConName(ref name) if name == "I#"));
}

#[test]
fn rust_result_does_not_fall_back_to_ok_err_constructors() {
    let mut table = DataConTable::new();
    insert(&mut table, 100, "Ok", 1, "User.Result.Ok");
    insert(&mut table, 101, "Err", 1, "User.Result.Err");

    let err = Result::<(), ()>::Ok(())
        .to_value(&table)
        .expect_err("Rust Result is represented only by Haskell Either");
    assert!(matches!(err, BridgeError::UnknownDataConName(ref name) if name == "Right"));

    let impostor = HaskellValue::Con(
        DataConId(100),
        vec![HaskellValue::Con(DataConId(9), vec![])],
    );
    assert!(matches!(
        Result::<(), ()>::from_value(&impostor, &table),
        Err(BridgeError::UnknownDataCon(_))
    ));
}

#[test]
fn text_slice_rejects_impostor_boxed_offsets() {
    let mut table = DataConTable::new();
    insert(&mut table, 100, "Text", 3, "Data.Text.Text");
    insert(&mut table, 200, "I#", 1, "UserDefined.I#");
    let impostor_offset =
        HaskellValue::Con(DataConId(200), vec![HaskellValue::Lit(Literal::LitInt(0))]);
    let text = HaskellValue::Con(
        DataConId(100),
        vec![
            HaskellValue::Lit(Literal::LitByteArray(b"x".to_vec())),
            impostor_offset,
            HaskellValue::Lit(Literal::LitInt(1)),
        ],
    );

    assert!(matches!(
        String::from_value(&text, &table),
        Err(BridgeError::TypeMismatch { .. })
    ));
}
