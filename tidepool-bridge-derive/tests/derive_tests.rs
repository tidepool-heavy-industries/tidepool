use tidepool_bridge::{BridgeError, FromCore, ToCore};
use tidepool_bridge_derive::{FromCore, ToCore};
use tidepool_eval::Value;
use tidepool_repr::{DataCon, DataConId, DataConTable};
use tidepool_testing::gen::datacon_table::standard_datacon_table;

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum MyBool {
    #[core(name = "True")]
    MyTrue,
    #[core(name = "False")]
    MyFalse,
}

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum MyMaybe<T> {
    #[core(name = "Nothing")]
    MyNothing,
    #[core(name = "Just")]
    MyJust(T),
}

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum MultiField {
    #[core(name = "Triple")]
    Triple(i64, bool, String),
}

/// A ZERO-ARITY TUPLE variant (`Budget()`, parens present) — distinct from a
/// unit variant (`MyBool::MyTrue` above, no parens). Regression coverage for
/// a #335 bug: `ToCore`'s pattern-match arm dropped the `()` for any
/// `rust_arity == 0` variant, so a nullary tuple constructor (e.g. an
/// `errors`-block ADT's `LlmBudget`) failed to compile.
#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum NullaryTuple {
    #[core(name = "Budget")]
    Budget(),
    #[core(name = "NullaryDetail")]
    Detail(String),
}

fn test_table() -> DataConTable {
    let mut t = standard_datacon_table();
    t.insert(DataCon {
        id: DataConId(20),
        name: "()".into(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(21),
        name: "Triple".into(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(22),
        name: "GetBranch".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(23),
        name: "UnitStruct".into(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(24),
        name: "Pair".into(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(25),
        name: "Budget".into(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    t.insert(DataCon {
        id: DataConId(26),
        name: "NullaryDetail".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    t
}

#[test]
fn test_bool_derive() {
    let table = test_table();
    let val = MyBool::MyTrue;
    let value = val.to_value(&table).unwrap();
    let back = MyBool::from_value(&value, &table).unwrap();
    assert_eq!(val, back);

    let val = MyBool::MyFalse;
    let value = val.to_value(&table).unwrap();
    let back = MyBool::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_maybe_derive() {
    let table = test_table();
    let val: MyMaybe<i64> = MyMaybe::MyJust(42);
    let value = val.to_value(&table).unwrap();
    let back = MyMaybe::<i64>::from_value(&value, &table).unwrap();
    assert_eq!(val, back);

    let val: MyMaybe<i64> = MyMaybe::MyNothing;
    let value = val.to_value(&table).unwrap();
    let back = MyMaybe::<i64>::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_multi_field_derive() {
    let table = test_table();
    let val = MultiField::Triple(42, true, "hello".into());
    let value = val.to_value(&table).unwrap();
    let back = MultiField::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

/// Regression for #335: `ToCore`'s generated pattern arm for a ZERO-ARITY
/// TUPLE variant (`Budget()`) used to drop the `()`, which doesn't compile —
/// `rust_arity == 0` alone doesn't distinguish a nullary tuple ctor from a
/// genuine unit variant. Round-trips both variants of `NullaryTuple`.
#[test]
fn test_nullary_tuple_variant_round_trip() {
    let table = test_table();
    let val = NullaryTuple::Budget();
    let value = val.to_value(&table).unwrap();
    let back = NullaryTuple::from_value(&value, &table).unwrap();
    assert_eq!(val, back);

    let val = NullaryTuple::Detail("over budget".to_string());
    let value = val.to_value(&table).unwrap();
    let back = NullaryTuple::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_generic_derive() {
    let table = test_table();
    let val: MyMaybe<MyMaybe<i64>> = MyMaybe::MyJust(MyMaybe::MyJust(42));
    let value = val.to_value(&table).unwrap();
    let back = MyMaybe::<MyMaybe<i64>>::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_unknown_variant() {
    let table = test_table();
    let value = Value::Con(DataConId(100), vec![]);
    let res = MyBool::from_value(&value, &table);
    assert!(matches!(
        res,
        Err(BridgeError::UnknownDataCon(DataConId(100)))
    ));
}

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]

enum UnusedParam<T> {
    #[core(name = "True")]
    Constant(std::marker::PhantomData<T>),
}

#[test]

fn test_unused_param_derive() {
    let table = test_table();

    // This should compile even if T doesn't implement FromCore/ToCore

    #[derive(Debug, PartialEq, Eq)]

    struct NotBridgeable;

    let val: UnusedParam<NotBridgeable> = UnusedParam::Constant(std::marker::PhantomData);

    let value = val.to_value(&table).unwrap();

    let back = UnusedParam::<NotBridgeable>::from_value(&value, &table).unwrap();

    assert_eq!(val, back);
}

#[test]

fn test_arity_mismatch() {
    let table = test_table();

    let true_id = table.get_by_name("True").unwrap();

    let value = Value::Con(true_id, vec![Value::Lit(tidepool_repr::Literal::LitInt(1))]);

    let res = MyBool::from_value(&value, &table);

    assert!(matches!(res, Err(BridgeError::ArityMismatch { .. })));
}

// --- Struct derive tests ---

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
#[core(name = "GetBranch")]
struct GetBranchRequest {
    working_dir: String,
}

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
struct UnitStruct;

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
#[core(name = "Pair")]
struct GenericStruct<A, B> {
    first: A,
    second: B,
}

#[test]
fn test_struct_single_field() {
    let table = test_table();
    let val = GetBranchRequest {
        working_dir: "/tmp".into(),
    };
    let value = val.to_value(&table).unwrap();
    let back = GetBranchRequest::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_struct_unit() {
    let table = test_table();
    let val = UnitStruct;
    let value = val.to_value(&table).unwrap();
    let back = UnitStruct::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_struct_generic() {
    let table = test_table();
    let val = GenericStruct {
        first: 42i64,
        second: true,
    };
    let value = val.to_value(&table).unwrap();
    let back = GenericStruct::<i64, bool>::from_value(&value, &table).unwrap();
    assert_eq!(val, back);
}

#[test]
fn test_struct_wrong_con() {
    let table = test_table();
    // Use Pair's constructor id with GetBranch's expected type
    let pair_id = table.get_by_name("Pair").unwrap();
    let value = Value::Con(pair_id, vec![Value::Lit(tidepool_repr::Literal::LitInt(1))]);
    let res = GetBranchRequest::from_value(&value, &table);
    assert!(matches!(res, Err(BridgeError::UnknownDataCon(_))));
}

#[test]
fn test_struct_arity_mismatch() {
    let table = test_table();
    let get_branch_id = table.get_by_name("GetBranch").unwrap();
    // GetBranch expects 1 field, give it 2
    let value = Value::Con(
        get_branch_id,
        vec![
            Value::Lit(tidepool_repr::Literal::LitInt(1)),
            Value::Lit(tidepool_repr::Literal::LitInt(2)),
        ],
    );
    let res = GetBranchRequest::from_value(&value, &table);
    assert!(matches!(res, Err(BridgeError::ArityMismatch { .. })));
}

// === F7: an EARLIER variant's missing DataCon must not fail-fast a LATER,
// present variant's decode ===

#[derive(Debug, PartialEq, Eq, FromCore, ToCore)]
enum TwoVariant {
    #[core(name = "FirstVariant")]
    First(i64),
    #[core(name = "SecondVariant")]
    Second(i64),
}

/// A table that registers ONLY `SecondVariant` — `FirstVariant` is absent
/// entirely (as if this compilation's table simply never carried it).
fn partial_two_variant_table() -> DataConTable {
    let mut t = standard_datacon_table();
    t.insert(DataCon {
        id: DataConId(50),
        name: "SecondVariant".into(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    t
}

/// Decoding a value of the LATER variant (`Second`) must succeed even though
/// the EARLIER variant's (`First`) constructor lookup fails — before the fix,
/// each variant's lookup ended in a bare `?`, so `First`'s
/// `UnknownDataConNameArity` aborted `from_value` before `Second` ever got a
/// chance, regardless of what the actual value was.
#[test]
fn later_variant_decodes_despite_earlier_variants_missing_constructor() {
    let table = partial_two_variant_table();
    let second_id = table.get_by_name("SecondVariant").unwrap();
    let value = Value::Con(
        second_id,
        vec![Value::Lit(tidepool_repr::Literal::LitInt(7))],
    );

    let decoded = TwoVariant::from_value(&value, &table)
        .expect("Second must decode even though First's constructor is absent from the table");
    assert_eq!(decoded, TwoVariant::Second(7));
}

/// A value that matches NEITHER variant (both because `First` isn't in the
/// table and this id genuinely isn't `Second`) is still a real decode
/// failure — the skip-on-missing-lookup fix must not swallow genuine errors.
#[test]
fn no_variant_matches_is_still_an_error() {
    let table = partial_two_variant_table();
    let unrelated_id = table.get_by_name("True").unwrap();
    let value = Value::Con(unrelated_id, vec![]);

    let res = TwoVariant::from_value(&value, &table);
    assert!(
        matches!(res, Err(BridgeError::UnknownDataCon(id)) if id == unrelated_id),
        "expected UnknownDataCon({unrelated_id:?}), got {res:?}"
    );
}
