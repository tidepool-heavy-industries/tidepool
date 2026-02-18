use super::WriteError;
use crate::frame::CoreFrame;
use crate::tree::RecursiveTree;
use crate::types::{Alt, AltCon, Literal, PrimOpKind};
use ciborium::value::Value;

/// Writes a CoreExpr to a CBOR-encoded byte vector.
pub fn write_cbor(expr: &RecursiveTree<CoreFrame<usize>>) -> Result<Vec<u8>, WriteError> {
    if expr.nodes.is_empty() {
        return Err(WriteError::Cbor(
            "attempted to write an empty RecursiveTree as a CoreExpr".to_string(),
        ));
    }

    let mut nodes_val = Vec::with_capacity(expr.nodes.len());
    for node in &expr.nodes {
        nodes_val.push(encode_frame(node));
    }

    let root_idx = (expr.nodes.len() - 1) as u64;

    let tree_val = Value::Array(vec![
        Value::Array(nodes_val),
        Value::Integer(root_idx.into()),
    ]);

    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&tree_val, &mut bytes)
        .map_err(|e| WriteError::Cbor(e.to_string()))?;
    Ok(bytes)
}

fn encode_frame(frame: &CoreFrame<usize>) -> Value {
    match frame {
        CoreFrame::Var(id) => Value::Array(vec![
            Value::Text("Var".to_string()),
            Value::Integer(id.0.into()),
        ]),
        CoreFrame::Lit(lit) => {
            Value::Array(vec![Value::Text("Lit".to_string()), encode_literal(lit)])
        }
        CoreFrame::App { fun, arg } => Value::Array(vec![
            Value::Text("App".to_string()),
            Value::Integer((*fun as u64).into()),
            Value::Integer((*arg as u64).into()),
        ]),
        CoreFrame::Lam { binder, body } => Value::Array(vec![
            Value::Text("Lam".to_string()),
            Value::Integer(binder.0.into()),
            Value::Integer((*body as u64).into()),
        ]),
        CoreFrame::LetNonRec { binder, rhs, body } => Value::Array(vec![
            Value::Text("LetNonRec".to_string()),
            Value::Integer(binder.0.into()),
            Value::Integer((*rhs as u64).into()),
            Value::Integer((*body as u64).into()),
        ]),
        CoreFrame::LetRec { bindings, body } => {
            let bindings_val = Value::Array(
                bindings
                    .iter()
                    .map(|(id, rhs)| {
                        Value::Array(vec![
                            Value::Integer(id.0.into()),
                            Value::Integer((*rhs as u64).into()),
                        ])
                    })
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("LetRec".to_string()),
                bindings_val,
                Value::Integer((*body as u64).into()),
            ])
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let alts_val = Value::Array(alts.iter().map(encode_alt).collect());
            Value::Array(vec![
                Value::Text("Case".to_string()),
                Value::Integer((*scrutinee as u64).into()),
                Value::Integer(binder.0.into()),
                alts_val,
            ])
        }
        CoreFrame::Con { tag, fields } => {
            let fields_val = Value::Array(
                fields
                    .iter()
                    .map(|f| Value::Integer((*f as u64).into()))
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("Con".to_string()),
                Value::Integer(tag.0.into()),
                fields_val,
            ])
        }
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
            let params_val =
                Value::Array(params.iter().map(|p| Value::Integer(p.0.into())).collect());
            Value::Array(vec![
                Value::Text("Join".to_string()),
                Value::Integer(label.0.into()),
                params_val,
                Value::Integer((*rhs as u64).into()),
                Value::Integer((*body as u64).into()),
            ])
        }
        CoreFrame::Jump { label, args } => {
            let args_val = Value::Array(
                args.iter()
                    .map(|a| Value::Integer((*a as u64).into()))
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("Jump".to_string()),
                Value::Integer(label.0.into()),
                args_val,
            ])
        }
        CoreFrame::PrimOp { op, args } => {
            let args_val = Value::Array(
                args.iter()
                    .map(|a| Value::Integer((*a as u64).into()))
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("PrimOp".to_string()),
                Value::Text(encode_primop(op).to_string()),
                args_val,
            ])
        }
    }
}

fn encode_primop(op: &PrimOpKind) -> &'static str {
    use PrimOpKind::*;
    match op {
        IntAdd => "IntAdd",
        IntSub => "IntSub",
        IntMul => "IntMul",
        IntNegate => "IntNegate",
        IntEq => "IntEq",
        IntNe => "IntNe",
        IntLt => "IntLt",
        IntLe => "IntLe",
        IntGt => "IntGt",
        IntGe => "IntGe",
        WordAdd => "WordAdd",
        WordSub => "WordSub",
        WordMul => "WordMul",
        WordEq => "WordEq",
        WordNe => "WordNe",
        WordLt => "WordLt",
        WordLe => "WordLe",
        WordGt => "WordGt",
        WordGe => "WordGe",
        DoubleAdd => "DoubleAdd",
        DoubleSub => "DoubleSub",
        DoubleMul => "DoubleMul",
        DoubleDiv => "DoubleDiv",
        DoubleEq => "DoubleEq",
        DoubleNe => "DoubleNe",
        DoubleLt => "DoubleLt",
        DoubleLe => "DoubleLe",
        DoubleGt => "DoubleGt",
        DoubleGe => "DoubleGe",
        CharEq => "CharEq",
        CharNe => "CharNe",
        CharLt => "CharLt",
        CharLe => "CharLe",
        CharGt => "CharGt",
        CharGe => "CharGe",
        IndexArray => "IndexArray",
        SeqOp => "SeqOp",
        TagToEnum => "TagToEnum",
        DataToTag => "DataToTag",
        IntQuot => "IntQuot",
        IntRem => "IntRem",
        Chr => "Chr",
        Ord => "Ord",
    }
}

fn encode_literal(lit: &Literal) -> Value {
    match lit {
        Literal::LitInt(i) => Value::Array(vec![
            Value::Text("LitInt".to_string()),
            Value::Integer((*i).into()),
        ]),
        Literal::LitWord(w) => Value::Array(vec![
            Value::Text("LitWord".to_string()),
            Value::Integer((*w).into()),
        ]),
        Literal::LitChar(c) => Value::Array(vec![
            Value::Text("LitChar".to_string()),
            Value::Integer((*c as u32).into()),
        ]),
        Literal::LitString(s) => Value::Array(vec![
            Value::Text("LitString".to_string()),
            Value::Bytes(s.clone()),
        ]),
        Literal::LitFloat(f) => Value::Array(vec![
            Value::Text("LitFloat".to_string()),
            Value::Integer((*f).into()),
        ]),
        Literal::LitDouble(d) => Value::Array(vec![
            Value::Text("LitDouble".to_string()),
            Value::Integer((*d).into()),
        ]),
    }
}

fn encode_alt(alt: &Alt<usize>) -> Value {
    let binders_val = Value::Array(
        alt.binders
            .iter()
            .map(|b| Value::Integer(b.0.into()))
            .collect(),
    );
    Value::Array(vec![
        encode_alt_con(&alt.con),
        binders_val,
        Value::Integer((alt.body as u64).into()),
    ])
}

fn encode_alt_con(con: &AltCon) -> Value {
    match con {
        AltCon::DataAlt(id) => Value::Array(vec![
            Value::Text("DataAlt".to_string()),
            Value::Integer(id.0.into()),
        ]),
        AltCon::LitAlt(lit) => {
            Value::Array(vec![Value::Text("LitAlt".to_string()), encode_literal(lit)])
        }
        AltCon::Default => Value::Array(vec![Value::Text("Default".to_string())]),
    }
}
