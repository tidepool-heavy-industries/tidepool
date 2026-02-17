use crate::error::BridgeError;
use crate::traits::{FromCore, ToCore};
use core_eval::Value;
use core_repr::{DataConTable, Literal};

// Helper for type mismatch errors
fn type_mismatch(expected: &str, got: &Value) -> BridgeError {
    let got_str = match got {
        Value::Lit(l) => format!("{:?}", l),
        Value::Con(id, _) => format!("Con({:?})", id),
        Value::Closure(_, _, _) => "Closure".to_string(),
        Value::ThunkRef(_) => "ThunkRef".to_string(),
        Value::JoinCont(_, _, _) => "JoinCont".to_string(),
    };
    BridgeError::TypeMismatch {
        expected: expected.to_string(),
        got: got_str,
    }
}

impl<T> FromCore for std::marker::PhantomData<T> {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(_, fields) if fields.is_empty() => Ok(std::marker::PhantomData),
            Value::Con(id, fields) => Err(BridgeError::ArityMismatch {
                con: *id,
                expected: 0,
                got: fields.len(),
            }),
            _ => Err(type_mismatch("Con", value)),
        }
    }
}

impl<T> ToCore for std::marker::PhantomData<T> {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        // We use a dummy id since PhantomData has no representation in Core
        // but we need some Con to represent it if it's a field.
        // Actually, in Tidepool/Haskell, PhantomData fields shouldn't exist in Core.
        // But for the bridge to work with derived enums, we need an impl.
        // Let's use a unit tuple id if available, or just any unit-like.
        let id = table
            .get_by_name("()")
            .or_else(|| table.iter().find(|dc| dc.rep_arity == 0).map(|dc| dc.id))
            .ok_or_else(|| BridgeError::UnknownDataConName("()".into()))?;
        Ok(Value::Con(id, vec![]))
    }
}

// Primitives

/// Bridges Rust `i64` to Haskell `Int#` literal.
impl FromCore for i64 {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitInt(n)) => Ok(*n),
            _ => Err(type_mismatch("LitInt", value)),
        }
    }
}

impl ToCore for i64 {
    fn to_value(&self, _table: &DataConTable) -> Result<Value, BridgeError> {
        Ok(Value::Lit(Literal::LitInt(*self)))
    }
}

/// Bridges Rust `u64` to Haskell `Word#` literal.
impl FromCore for u64 {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitWord(n)) => Ok(*n),
            _ => Err(type_mismatch("LitWord", value)),
        }
    }
}

impl ToCore for u64 {
    fn to_value(&self, _table: &DataConTable) -> Result<Value, BridgeError> {
        Ok(Value::Lit(Literal::LitWord(*self)))
    }
}

/// Bridges Rust `f64` to Haskell `Double#` literal.
impl FromCore for f64 {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitDouble(bits)) => Ok(f64::from_bits(*bits)),
            _ => Err(type_mismatch("LitDouble", value)),
        }
    }
}

impl ToCore for f64 {
    fn to_value(&self, _table: &DataConTable) -> Result<Value, BridgeError> {
        Ok(Value::Lit(Literal::LitDouble(self.to_bits())))
    }
}

/// Bridges Rust `i32` to Haskell `Int#` literal.
/// Returns error on overflow/underflow.
impl FromCore for i32 {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        let n = i64::from_value(value, table)?;
        if n < i32::MIN as i64 || n > i32::MAX as i64 {
            return Err(BridgeError::TypeMismatch {
                expected: "i32 in range [-2147483648, 2147483647]".to_string(),
                got: format!("LitInt({})", n),
            });
        }
        Ok(n as i32)
    }
}

impl ToCore for i32 {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        (*self as i64).to_value(table)
    }
}

/// Bridges Rust `bool` to Haskell `Bool` (True/False constructors).
impl FromCore for bool {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(id, fields) => {
                let true_id = table
                    .get_by_name("True")
                    .ok_or(BridgeError::UnknownDataConName("True".into()))?;
                let false_id = table
                    .get_by_name("False")
                    .ok_or(BridgeError::UnknownDataConName("False".into()))?;

                if *id == true_id {
                    if fields.is_empty() {
                        Ok(true)
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 0,
                            got: fields.len(),
                        })
                    }
                } else if *id == false_id {
                    if fields.is_empty() {
                        Ok(false)
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 0,
                            got: fields.len(),
                        })
                    }
                } else {
                    Err(BridgeError::UnknownDataCon(*id))
                }
            }
            _ => Err(type_mismatch("Con", value)),
        }
    }
}

impl ToCore for bool {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let name = if *self { "True" } else { "False" };
        let id = table
            .get_by_name(name)
            .ok_or_else(|| BridgeError::UnknownDataConName(name.into()))?;
        Ok(Value::Con(id, vec![]))
    }
}

impl FromCore for char {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitChar(c)) => Ok(*c),
            _ => Err(type_mismatch("LitChar", value)),
        }
    }
}

impl ToCore for char {
    fn to_value(&self, _table: &DataConTable) -> Result<Value, BridgeError> {
        Ok(Value::Lit(Literal::LitChar(*self)))
    }
}

impl FromCore for String {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitString(bytes)) => {
                String::from_utf8(bytes.clone()).map_err(|e| BridgeError::TypeMismatch {
                    expected: "UTF-8 String".to_string(),
                    got: format!("Invalid UTF-8: {}", e),
                })
            }
            _ => Err(type_mismatch("LitString", value)),
        }
    }
}

impl ToCore for String {
    fn to_value(&self, _table: &DataConTable) -> Result<Value, BridgeError> {
        Ok(Value::Lit(Literal::LitString(self.as_bytes().to_vec())))
    }
}

// Containers

impl<T: FromCore> FromCore for Option<T> {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(id, fields) => {
                let nothing_id = table
                    .get_by_name("Nothing")
                    .ok_or(BridgeError::UnknownDataConName("Nothing".into()))?;
                let just_id = table
                    .get_by_name("Just")
                    .ok_or(BridgeError::UnknownDataConName("Just".into()))?;

                if *id == nothing_id {
                    if fields.is_empty() {
                        Ok(None)
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 0,
                            got: fields.len(),
                        })
                    }
                } else if *id == just_id {
                    if fields.len() == 1 {
                        Ok(Some(T::from_value(&fields[0], table)?))
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 1,
                            got: fields.len(),
                        })
                    }
                } else {
                    Err(BridgeError::UnknownDataCon(*id))
                }
            }
            _ => Err(type_mismatch("Con", value)),
        }
    }
}

impl<T: ToCore> ToCore for Option<T> {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        match self {
            None => {
                let id = table
                    .get_by_name("Nothing")
                    .ok_or_else(|| BridgeError::UnknownDataConName("Nothing".into()))?;
                Ok(Value::Con(id, vec![]))
            }
            Some(x) => {
                let id = table
                    .get_by_name("Just")
                    .ok_or_else(|| BridgeError::UnknownDataConName("Just".into()))?;
                Ok(Value::Con(id, vec![x.to_value(table)?]))
            }
        }
    }
}

impl<T: FromCore> FromCore for Vec<T> {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        let nil_id = table
            .get_by_name("[]")
            .ok_or(BridgeError::UnknownDataConName("[]".into()))?;
        let cons_id = table
            .get_by_name(":")
            .ok_or(BridgeError::UnknownDataConName(":".into()))?;

        let mut res = Vec::new();
        let mut curr = value;

        loop {
            match curr {
                Value::Con(id, fields) => {
                    if *id == nil_id {
                        if fields.is_empty() {
                            break;
                        } else {
                            return Err(BridgeError::ArityMismatch {
                                con: *id,
                                expected: 0,
                                got: fields.len(),
                            });
                        }
                    } else if *id == cons_id {
                        if fields.len() == 2 {
                            res.push(T::from_value(&fields[0], table)?);
                            curr = &fields[1];
                        } else {
                            return Err(BridgeError::ArityMismatch {
                                con: *id,
                                expected: 2,
                                got: fields.len(),
                            });
                        }
                    } else {
                        return Err(BridgeError::UnknownDataCon(*id));
                    }
                }
                _ => return Err(type_mismatch("Con", curr)),
            }
        }

        Ok(res)
    }
}

impl<T: ToCore> ToCore for Vec<T> {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let nil_id = table
            .get_by_name("[]")
            .ok_or_else(|| BridgeError::UnknownDataConName("[]".into()))?;
        let cons_id = table
            .get_by_name(":")
            .ok_or_else(|| BridgeError::UnknownDataConName(":".into()))?;

        let mut res = Value::Con(nil_id, vec![]);
        for x in self.iter().rev() {
            res = Value::Con(cons_id, vec![x.to_value(table)?, res]);
        }
        Ok(res)
    }
}

impl<T: FromCore, E: FromCore> FromCore for Result<T, E> {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(id, fields) => {
                let right_id = table
                    .get_by_name("Right")
                    .or_else(|| table.get_by_name("Ok"))
                    .ok_or(BridgeError::UnknownDataConName("Right/Ok".into()))?;
                let left_id = table
                    .get_by_name("Left")
                    .or_else(|| table.get_by_name("Err"))
                    .ok_or(BridgeError::UnknownDataConName("Left/Err".into()))?;

                if *id == right_id {
                    if fields.len() == 1 {
                        Ok(Ok(T::from_value(&fields[0], table)?))
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 1,
                            got: fields.len(),
                        })
                    }
                } else if *id == left_id {
                    if fields.len() == 1 {
                        Ok(Err(E::from_value(&fields[0], table)?))
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 1,
                            got: fields.len(),
                        })
                    }
                } else {
                    Err(BridgeError::UnknownDataCon(*id))
                }
            }
            _ => Err(type_mismatch("Con", value)),
        }
    }
}

impl<T: ToCore, E: ToCore> ToCore for Result<T, E> {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        match self {
            Ok(x) => {
                let id = table
                    .get_by_name("Right")
                    .or_else(|| table.get_by_name("Ok"))
                    .ok_or_else(|| BridgeError::UnknownDataConName("Right/Ok".into()))?;
                Ok(Value::Con(id, vec![x.to_value(table)?]))
            }
            Err(e) => {
                let id = table
                    .get_by_name("Left")
                    .or_else(|| table.get_by_name("Err"))
                    .ok_or_else(|| BridgeError::UnknownDataConName("Left/Err".into()))?;
                Ok(Value::Con(id, vec![e.to_value(table)?]))
            }
        }
    }
}

// Tuples

impl<A: FromCore, B: FromCore> FromCore for (A, B) {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(id, fields) => {
                let pair_id = table
                    .get_by_name("(,)")
                    .ok_or(BridgeError::UnknownDataConName("(,)".into()))?;
                if *id == pair_id {
                    if fields.len() == 2 {
                        Ok((
                            A::from_value(&fields[0], table)?,
                            B::from_value(&fields[1], table)?,
                        ))
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 2,
                            got: fields.len(),
                        })
                    }
                } else {
                    Err(BridgeError::UnknownDataCon(*id))
                }
            }
            _ => Err(type_mismatch("Con", value)),
        }
    }
}

impl<A: ToCore, B: ToCore> ToCore for (A, B) {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let pair_id = table
            .get_by_name("(,)")
            .ok_or_else(|| BridgeError::UnknownDataConName("(,)".into()))?;
        Ok(Value::Con(
            pair_id,
            vec![self.0.to_value(table)?, self.1.to_value(table)?],
        ))
    }
}

impl<A: FromCore, B: FromCore, C: FromCore> FromCore for (A, B, C) {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(id, fields) => {
                let triple_id = table
                    .get_by_name("(,,)")
                    .ok_or(BridgeError::UnknownDataConName("(,,)".into()))?;
                if *id == triple_id {
                    if fields.len() == 3 {
                        Ok((
                            A::from_value(&fields[0], table)?,
                            B::from_value(&fields[1], table)?,
                            C::from_value(&fields[2], table)?,
                        ))
                    } else {
                        Err(BridgeError::ArityMismatch {
                            con: *id,
                            expected: 3,
                            got: fields.len(),
                        })
                    }
                } else {
                    Err(BridgeError::UnknownDataCon(*id))
                }
            }
            _ => Err(type_mismatch("Con", value)),
        }
    }
}

impl<A: ToCore, B: ToCore, C: ToCore> ToCore for (A, B, C) {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let triple_id = table
            .get_by_name("(,,)")
            .ok_or_else(|| BridgeError::UnknownDataConName("(,,)".into()))?;
        Ok(Value::Con(
            triple_id,
            vec![
                self.0.to_value(table)?,
                self.1.to_value(table)?,
                self.2.to_value(table)?,
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_repr::{DataCon, DataConId};

    fn test_table() -> DataConTable {
        let mut t = DataConTable::new();
        // Nothing=0, Just=1, False=2, True=3, ()=4, Nil=5, Cons=6, (,,)=7, Right=8, Left=9
        t.insert(DataCon {
            id: DataConId(0),
            name: "Nothing".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(1),
            name: "Just".into(),
            tag: 2,
            rep_arity: 1,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(2),
            name: "False".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(3),
            name: "True".into(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(4),
            name: "(,)".into(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(5),
            name: "[]".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(6),
            name: ":".into(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(7),
            name: "(,,)".into(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(8),
            name: "Right".into(),
            tag: 2,
            rep_arity: 1,
            field_bangs: vec![],
        });
        t.insert(DataCon {
            id: DataConId(9),
            name: "Left".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
        });
        t
    }

    fn roundtrip<T: FromCore + ToCore + PartialEq + std::fmt::Debug>(val: T, table: &DataConTable) {
        let value = val.to_value(table).expect("ToValue failed");
        let back = T::from_value(&value, table).expect("FromValue failed");
        assert_eq!(val, back);
    }

    #[test]
    fn test_i64_roundtrip() {
        let table = test_table();
        roundtrip(42i64, &table);
        roundtrip(-7i64, &table);
    }

    #[test]
    fn test_i32_roundtrip() {
        let table = test_table();
        roundtrip(42i32, &table);
        roundtrip(-7i32, &table);
    }

    #[test]
    fn test_i32_overflow() {
        let table = test_table();
        let val: i64 = i32::MAX as i64 + 1;
        let value = val.to_value(&table).unwrap();
        let res = i32::from_value(&value, &table);
        assert!(matches!(res, Err(BridgeError::TypeMismatch { .. })));
    }

    #[test]
    fn test_u64_roundtrip() {
        let table = test_table();
        roundtrip(42u64, &table);
    }

    #[test]
    fn test_f64_roundtrip() {
        let table = test_table();
        roundtrip(3.14159f64, &table);
        roundtrip(-0.0f64, &table);
    }

    #[test]
    fn test_bool_roundtrip() {
        let table = test_table();
        roundtrip(true, &table);
        roundtrip(false, &table);
    }

    #[test]
    fn test_char_roundtrip() {
        let table = test_table();
        roundtrip('a', &table);
        roundtrip('λ', &table);
    }

    #[test]
    fn test_string_roundtrip() {
        let table = test_table();
        roundtrip("hello".to_string(), &table);
        roundtrip("".to_string(), &table);
    }

    #[test]
    fn test_option_roundtrip() {
        let table = test_table();
        roundtrip(Some(42i64), &table);
        roundtrip(None::<i64>, &table);
    }

    #[test]
    fn test_vec_roundtrip() {
        let table = test_table();
        roundtrip(vec![1i64, 2, 3], &table);
        roundtrip(Vec::<i64>::new(), &table);
    }

    #[test]
    fn test_result_roundtrip() {
        let table = test_table();
        roundtrip(Ok::<i64, String>(42), &table);
        roundtrip(Err::<i64, String>("error".to_string()), &table);
    }

    #[test]
    fn test_tuple2_roundtrip() {
        let table = test_table();
        roundtrip((42i64, true), &table);
    }

    #[test]
    fn test_tuple3_roundtrip() {
        let table = test_table();
        roundtrip((42i64, true, "hello".to_string()), &table);
    }

    #[test]
    fn test_nested_roundtrip() {
        let table = test_table();
        roundtrip(vec![Some(1i64), None, Some(3)], &table);
        roundtrip(Some((42i64, vec![true, false])), &table);
    }

    #[test]
    fn test_unknown_datacon() {
        let table = test_table();
        let value = Value::Con(DataConId(100), vec![]);
        let res = bool::from_value(&value, &table);
        assert!(matches!(
            res,
            Err(BridgeError::UnknownDataCon(DataConId(100)))
        ));
    }

    #[test]
    fn test_arity_mismatch() {
        let table = test_table();
        let true_id = table.get_by_name("True").unwrap();
        let value = Value::Con(true_id, vec![Value::Lit(Literal::LitInt(1))]);
        let res = bool::from_value(&value, &table);
        assert!(matches!(res, Err(BridgeError::ArityMismatch { .. })));
    }

    #[test]
    fn test_type_mismatch() {
        let table = test_table();
        let value = Value::Lit(Literal::LitInt(1));
        let res = bool::from_value(&value, &table);
        assert!(matches!(res, Err(BridgeError::TypeMismatch { .. })));
    }
}
