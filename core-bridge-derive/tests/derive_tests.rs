use core_bridge::{BridgeError, FromCore, ToCore};
use core_bridge_derive::{FromCore, ToCore};
use core_eval::Value;
use core_repr::{DataCon, DataConId, DataConTable};
use core_testing::gen::datacon_table::standard_datacon_table;

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

fn test_table() -> DataConTable {
    let mut t = standard_datacon_table();
    t.insert(DataCon {
        id: DataConId(4),
        name: "()".into(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
    });
    t.insert(DataCon {
        id: DataConId(10),
        name: "Triple".into(),
        tag: 1,
        rep_arity: 3,
        field_bangs: vec![],
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
    assert!(matches!(res, Err(BridgeError::UnknownDataCon(DataConId(100)))));
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

    let value = Value::Con(true_id, vec![Value::Lit(core_repr::Literal::LitInt(1))]);

    let res = MyBool::from_value(&value, &table);

    assert!(matches!(res, Err(BridgeError::ArityMismatch { .. })));

}


