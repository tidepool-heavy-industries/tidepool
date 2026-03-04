use crate::error::BridgeError;
use crate::traits::{FromCore, ToCore};
use std::sync::{Arc, Mutex};
use tidepool_eval::Value;
use tidepool_repr::{DataConId, DataConTable, Literal};

/// Check if a DataConId matches a known boxing constructor name (I#, W#, D#, C#).
fn is_boxing_con(name: &str, id: DataConId, table: &DataConTable) -> bool {
    table.get_by_name(name) == Some(id)
}

// Helper for type mismatch errors
fn type_mismatch(expected: &str, got: &Value) -> BridgeError {
    let got_str = match got {
        Value::Lit(l) => format!("{:?}", l),
        Value::Con(id, _) => format!("Con({:?})", id),
        Value::Closure(_, _, _) => "Closure".to_string(),
        Value::ThunkRef(_) => "ThunkRef".to_string(),
        Value::JoinCont(_, _, _) => "JoinCont".to_string(),
        Value::ConFun(id, arity, args) => format!("ConFun({:?}, {}/{})", id, args.len(), arity),
        Value::ByteArray(bs) => format!("ByteArray(len={})", bs.lock().unwrap().len()),
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

// Box

// Value identity — pass through without conversion.
impl ToCore for Value {
    fn to_value(&self, _table: &DataConTable) -> Result<Value, BridgeError> {
        Ok(self.clone())
    }
}

impl FromCore for Value {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        Ok(value.clone())
    }
}

impl<T: FromCore> FromCore for Box<T> {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        T::from_value(value, table).map(Box::new)
    }
}

impl<T: ToCore> ToCore for Box<T> {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        (**self).to_value(table)
    }
}

// Unit

impl ToCore for () {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let id = table
            .get_by_name("()")
            .ok_or_else(|| BridgeError::UnknownDataConName("()".into()))?;
        Ok(Value::Con(id, vec![]))
    }
}

impl FromCore for () {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(_, fields) if fields.is_empty() => Ok(()),
            _ => Err(type_mismatch("()", value)),
        }
    }
}

// Primitives

/// Bridges Rust `i64` to Haskell `Int#` literal.
/// Also transparently unwraps `I#(n)` (boxed Int).
impl FromCore for i64 {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitInt(n)) => Ok(*n),
            Value::Con(id, fields) if fields.len() == 1 => {
                if is_boxing_con("I#", *id, table) {
                    i64::from_value(&fields[0], table)
                } else {
                    Err(type_mismatch("LitInt or I#", value))
                }
            }
            _ => Err(type_mismatch("LitInt or I#", value)),
        }
    }
}

impl ToCore for i64 {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let id = table
            .get_by_name("I#")
            .ok_or_else(|| BridgeError::UnknownDataConName("I#".into()))?;
        Ok(Value::Con(id, vec![Value::Lit(Literal::LitInt(*self))]))
    }
}

/// Bridges Rust `u64` to Haskell `Word#` literal.
/// Also transparently unwraps `W#(n)` (boxed Word).
impl FromCore for u64 {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitWord(n)) => Ok(*n),
            Value::Con(id, fields) if fields.len() == 1 => {
                if is_boxing_con("W#", *id, table) {
                    u64::from_value(&fields[0], table)
                } else {
                    Err(type_mismatch("LitWord or W#", value))
                }
            }
            _ => Err(type_mismatch("LitWord or W#", value)),
        }
    }
}

impl ToCore for u64 {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let id = table
            .get_by_name("W#")
            .ok_or_else(|| BridgeError::UnknownDataConName("W#".into()))?;
        Ok(Value::Con(id, vec![Value::Lit(Literal::LitWord(*self))]))
    }
}

/// Bridges Rust `f64` to Haskell `Double#` literal.
/// Also transparently unwraps `D#(n)` (boxed Double).
impl FromCore for f64 {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitDouble(bits)) => Ok(f64::from_bits(*bits)),
            Value::Con(id, fields) if fields.len() == 1 => {
                if is_boxing_con("D#", *id, table) {
                    f64::from_value(&fields[0], table)
                } else {
                    Err(type_mismatch("LitDouble or D#", value))
                }
            }
            _ => Err(type_mismatch("LitDouble or D#", value)),
        }
    }
}

impl ToCore for f64 {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let id = table
            .get_by_name("D#")
            .ok_or_else(|| BridgeError::UnknownDataConName("D#".into()))?;
        Ok(Value::Con(
            id,
            vec![Value::Lit(Literal::LitDouble(self.to_bits()))],
        ))
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

/// Also transparently unwraps `C#(c)` (boxed Char).
impl FromCore for char {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Lit(Literal::LitChar(c)) => Ok(*c),
            Value::Con(id, fields) if fields.len() == 1 => {
                if is_boxing_con("C#", *id, table) {
                    char::from_value(&fields[0], table)
                } else {
                    Err(type_mismatch("LitChar or C#", value))
                }
            }
            _ => Err(type_mismatch("LitChar or C#", value)),
        }
    }
}

impl ToCore for char {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let id = table
            .get_by_name("C#")
            .ok_or_else(|| BridgeError::UnknownDataConName("C#".into()))?;
        Ok(Value::Con(id, vec![Value::Lit(Literal::LitChar(*self))]))
    }
}

impl FromCore for String {
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            // Text constructor: Text ByteArray# off len
            Value::Con(id, fields)
                if fields.len() == 3 && table.get_by_name("Text") == Some(*id) =>
            {
                let ba = match &fields[0] {
                    Value::ByteArray(bs) => bs.lock().unwrap().clone(),
                    // Lifted ByteArray wrapper: Con("ByteArray", [Value::ByteArray(..)])
                    Value::Con(ba_id, ba_fields)
                        if ba_fields.len() == 1
                            && table.get_by_name("ByteArray") == Some(*ba_id) =>
                    {
                        match &ba_fields[0] {
                            Value::ByteArray(bs) => bs.lock().unwrap().clone(),
                            _ => {
                                return Err(type_mismatch("ByteArray# in ByteArray", &ba_fields[0]))
                            }
                        }
                    }
                    _ => return Err(type_mismatch("ByteArray or ByteArray# in Text", &fields[0])),
                };
                let off = i64::from_value(&fields[1], table)? as usize;
                let len = i64::from_value(&fields[2], table)? as usize;
                if off + len > ba.len() {
                    return Err(BridgeError::TypeMismatch {
                        expected: "valid Text slice".to_string(),
                        got: format!("off={}, len={}, ba_len={}", off, len, ba.len()),
                    });
                }
                String::from_utf8(ba[off..off + len].to_vec()).map_err(|e| {
                    BridgeError::TypeMismatch {
                        expected: "UTF-8 Text".to_string(),
                        got: format!("Invalid UTF-8: {}", e),
                    }
                })
            }
            Value::Lit(Literal::LitString(bytes)) => {
                String::from_utf8(bytes.clone()).map_err(|e| BridgeError::TypeMismatch {
                    expected: "UTF-8 String".to_string(),
                    got: format!("Invalid UTF-8: {}", e),
                })
            }
            // Also accept cons-cell list of Char (from ++ desugaring)
            Value::Con(_, _) => {
                let mut chars = Vec::new();
                let mut cur = value;
                loop {
                    match cur {
                        Value::Con(tag, fields)
                            if table.get_by_name("[]") == Some(*tag) && fields.is_empty() =>
                        {
                            break;
                        }
                        Value::Con(tag, fields)
                            if table.get_by_name(":") == Some(*tag) && fields.len() == 2 =>
                        {
                            match &fields[0] {
                                Value::Lit(Literal::LitChar(c)) => chars.push(*c),
                                // Boxing: C# wraps a Char
                                Value::Con(box_tag, box_fields)
                                    if table.get_by_name("C#") == Some(*box_tag)
                                        && box_fields.len() == 1 =>
                                {
                                    match &box_fields[0] {
                                        Value::Lit(Literal::LitChar(c)) => chars.push(*c),
                                        other => return Err(type_mismatch("Char in C#", other)),
                                    }
                                }
                                other => return Err(type_mismatch("Char or C#", other)),
                            }
                            cur = &fields[1];
                        }
                        _ => return Err(type_mismatch("[] or (:)", cur)),
                    }
                }
                Ok(chars.into_iter().collect())
            }
            _ => Err(type_mismatch("Text, LitString, or [Char]", value)),
        }
    }
}

impl ToCore for String {
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError> {
        let text_id = table
            .get_by_name("Text")
            .ok_or_else(|| BridgeError::UnknownDataConName("Text".into()))?;
        let bytes = self.as_bytes().to_vec();
        let len = bytes.len() as i64;
        // GHC Core at -O2 uses the worker representation of Text with
        // unboxed fields: Text ByteArray# Int# Int#
        // (not the source-level Text !ByteArray !Int !Int with boxed wrappers).
        // The JIT compiles GHC Core directly, so values injected from Rust
        // must match the worker representation.
        let ba_raw = Value::ByteArray(Arc::new(Mutex::new(bytes)));
        Ok(Value::Con(
            text_id,
            vec![
                ba_raw,
                Value::Lit(Literal::LitInt(0)),
                Value::Lit(Literal::LitInt(len)),
            ],
        ))
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
    use tidepool_repr::{DataCon, DataConId};

    fn test_table() -> DataConTable {
        let mut t = DataConTable::new();
        // Nothing=0, Just=1, False=2, True=3, ()=4, Nil=5, Cons=6, (,,)=7, Right=8, Left=9
        // I#=10, W#=11, D#=12, C#=13
        t.insert(DataCon {
            id: DataConId(0),
            name: "Nothing".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(1),
            name: "Just".into(),
            tag: 2,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(2),
            name: "False".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(3),
            name: "True".into(),
            tag: 2,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(4),
            name: "(,)".into(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(5),
            name: "[]".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(6),
            name: ":".into(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(7),
            name: "(,,)".into(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(8),
            name: "Right".into(),
            tag: 2,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(9),
            name: "Left".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(10),
            name: "I#".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(11),
            name: "W#".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(12),
            name: "D#".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(13),
            name: "C#".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(14),
            name: "()".into(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: None,
        });
        t.insert(DataCon {
            id: DataConId(15),
            name: "Text".into(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: None,
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

    #[test]
    fn test_string_unboxed_fields() {
        let table = test_table();
        let s = "hello".to_string();
        let value = s.to_value(&table).expect("ToValue failed");

        if let Value::Con(id, fields) = &value {
            assert_eq!(table.name_of(*id), Some("Text"));
            assert_eq!(fields.len(), 3);
            // First field: ByteArray# (unboxed)
            assert!(matches!(fields[0], Value::ByteArray(_)));
            // Second field: Int# 0 (unboxed literal)
            assert!(matches!(fields[1], Value::Lit(Literal::LitInt(0))));
            // Third field: Int# len (unboxed literal)
            assert!(matches!(fields[2], Value::Lit(Literal::LitInt(5))));
        } else {
            panic!("Expected Con, got {:?}", value);
        }
    }

    #[test]
    fn test_unit_roundtrip() {
        let table = test_table();
        roundtrip((), &table);
    }

    #[test]
    fn test_f64_boxed_roundtrip() {
        let table = test_table();
        roundtrip(3.14159f64, &table);
    }

    #[test]
    fn test_u64_boxed_roundtrip() {
        let table = test_table();
        roundtrip(42u64, &table);
    }

    #[test]
    fn test_char_boxed_roundtrip() {
        let table = test_table();
        roundtrip('a', &table);
    }

    #[test]
    fn test_vec_string_roundtrip() {
        let table = test_table();
        roundtrip(vec!["a".to_string(), "b".to_string()], &table);
    }

    #[test]
    fn test_option_nested_roundtrip() {
        let table = test_table();
        roundtrip(Some(vec![1i64, 2]), &table);
        roundtrip(None::<Vec<i64>>, &table);
    }

    #[test]
    fn test_result_nested_roundtrip() {
        let table = test_table();
        roundtrip(Ok::<Vec<i64>, String>(vec![1, 2]), &table);
        roundtrip(Err::<Vec<i64>, String>("error".to_string()), &table);
    }
}
