// The `miette::Diagnostic` derive on `TideParseError` below expands into code
// that trips `unused_assignments` on that struct's own field declarations
// (the derive's generated impl shadows the field names internally) — a
// false positive from the derive expansion, not from code in this module.
#![allow(unused_assignments)]

use crate::ast::{BinOp, BuiltinId, TExpr};
use miette::{Diagnostic, SourceSpan};
use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;
use thiserror::Error;

#[derive(Parser)]
#[grammar = "tide.pest"]
pub struct TideParser;

#[derive(Error, Diagnostic, Debug)]
#[error("Parse error")]
#[diagnostic(code(tide::parse_error), help("Check your syntax!"))]
pub struct TideParseError {
    #[source_code]
    pub src: String,

    #[label("here")]
    pub span: SourceSpan,

    pub message: String,
}

pub fn parse(input: &str) -> miette::Result<TExpr> {
    let pairs = TideParser::parse(Rule::program, input).map_err(|e| {
        let (line, col) = match e.line_col {
            pest::error::LineColLocation::Pos((l, c)) => (l, c),
            pest::error::LineColLocation::Span((l, c), _) => (l, c),
        };

        // Rough conversion of pest error to miette span.
        // Pest errors already have a good display, but miette makes it fancy.
        let offset = match e.location {
            pest::error::InputLocation::Pos(p) => p,
            pest::error::InputLocation::Span((start, _)) => start,
        };

        TideParseError {
            src: input.to_string(),
            span: (offset, 0).into(),
            message: format!("at line {}, column {}: {}", line, col, e.variant.message()),
        }
    })?;

    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees this rule has at least one inner pair; an empty match here is a grammar bug, not a runtime input condition"
    )]
    let pair = pairs.into_iter().next().unwrap();
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees this rule has at least one inner pair; an empty match here is a grammar bug, not a runtime input condition"
    )]
    parse_expr(pair.into_inner().next().unwrap())
        .map_err(|e| miette::miette!("Structural parse error: {}", e))
}

fn parse_expr(pair: Pair<Rule>) -> Result<TExpr, String> {
    match pair.as_rule() {
        Rule::let_expr => parse_let(pair),
        Rule::if_expr => parse_if(pair),
        Rule::lambda_expr => parse_lambda(pair),
        Rule::comparison => parse_comparison(pair),
        #[allow(
            clippy::unwrap_used,
            reason = "pest grammar guarantees this rule has at least one inner pair; an empty match here is a grammar bug, not a runtime input condition"
        )]
        Rule::expr => parse_expr(pair.into_inner().next().unwrap()),
        _ => Err(format!(
            "Unexpected rule in parse_expr: {:?}",
            pair.as_rule()
        )),
    }
}

fn parse_let(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees let_expr has an identifier pair"
    )]
    let ident = inner.next().unwrap().as_str().to_string();
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees let_expr has a value pair"
    )]
    let val = parse_expr(inner.next().unwrap())?;
    if let Some(body_pair) = inner.next() {
        let body = parse_expr(body_pair)?;
        Ok(TExpr::TLet(ident, Box::new(val), Box::new(body)))
    } else {
        // REPL shorthand: `let x = 5` → persistent binding (no restore)
        Ok(TExpr::TBind(ident, Box::new(val)))
    }
}

fn parse_if(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees if_expr has a condition pair"
    )]
    let cond = parse_expr(inner.next().unwrap())?;
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees if_expr has a then-branch pair"
    )]
    let t = parse_expr(inner.next().unwrap())?;
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees if_expr has an else-branch pair"
    )]
    let e = parse_expr(inner.next().unwrap())?;
    Ok(TExpr::TIf(Box::new(cond), Box::new(t), Box::new(e)))
}

fn parse_lambda(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner().collect::<Vec<_>>();
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees lambda_expr has a body pair"
    )]
    let body_pair = inner.pop().unwrap();
    let params = inner.into_iter().map(|p| p.as_str().to_string()).collect();
    let body = parse_expr(body_pair)?;
    Ok(TExpr::TLam(params, Box::new(body)))
}

fn parse_comparison(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_concat, Rule::comp_op, map_comp_op)
}

fn parse_concat(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_addition, Rule::concat_op, |_| BinOp::Concat)
}

fn parse_addition(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_multiplication, Rule::add_op, |s| {
        if s == "+" {
            BinOp::Add
        } else {
            BinOp::Sub
        }
    })
}

fn parse_multiplication(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_unary, Rule::mul_op, |s| {
        if s == "*" {
            BinOp::Mul
        } else {
            BinOp::Div
        }
    })
}

fn parse_binary_op<F, M>(
    pair: Pair<Rule>,
    next: F,
    op_rule: Rule,
    mapper: M,
) -> Result<TExpr, String>
where
    F: Fn(Pair<Rule>) -> Result<TExpr, String>,
    M: Fn(&str) -> BinOp,
{
    let mut inner = pair.into_inner();
    let first = inner
        .next()
        .ok_or_else(|| "Missing left operand".to_string())?;
    let mut left = next(first)?;

    while let Some(op_pair) = inner.next() {
        let op_str = if op_pair.as_rule() == op_rule {
            op_pair.as_str()
        } else {
            // Some rules like comp_op have nested ops
            #[allow(
                clippy::unwrap_used,
                reason = "pest grammar guarantees a nested-op rule has an inner op pair"
            )]
            op_pair.into_inner().next().unwrap().as_str()
        };
        let op = mapper(op_str);
        #[allow(
            clippy::unwrap_used,
            reason = "pest grammar guarantees a binary-op rule has a right-hand pair"
        )]
        let right = next(inner.next().unwrap())?;
        left = TExpr::TBinOp(op, Box::new(left), Box::new(right));
    }

    Ok(left)
}

fn map_comp_op(op: &str) -> BinOp {
    match op {
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "<" => BinOp::Lt,
        ">" => BinOp::Gt,
        "<=" => BinOp::Le,
        ">=" => BinOp::Ge,
        _ => BinOp::Add, // Should not happen with valid grammar
    }
}

fn parse_unary(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees a unary rule has at least one inner pair"
    )]
    let first = inner.next().unwrap();
    if first.as_rule() == Rule::neg_op {
        #[allow(
            clippy::unwrap_used,
            reason = "neg_op is only matched when a following operand pair exists"
        )]
        let val = parse_unary(inner.next().unwrap())?;
        Ok(TExpr::TBinOp(
            BinOp::Sub,
            Box::new(TExpr::TInt(0)),
            Box::new(val),
        ))
    } else {
        parse_call(first)
    }
}

fn parse_call(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees a call rule has an atom pair"
    )]
    let atom_pair = inner.next().unwrap();
    let mut current = parse_atom(atom_pair)?;

    for args_pair in inner {
        let args = parse_arg_list(args_pair)?;

        // Builtin detection: if the callee is an identifier and it matches a builtin name
        if let TExpr::TVar(ref name) = current {
            if let Some(id) = map_builtin(name) {
                current = TExpr::TBuiltin(id, args);
                continue;
            }
        }

        current = TExpr::TApp(Box::new(current), args);
    }

    Ok(current)
}

fn parse_arg_list(pair: Pair<Rule>) -> Result<Vec<TExpr>, String> {
    pair.into_inner().map(parse_expr).collect()
}

fn parse_atom(pair: Pair<Rule>) -> Result<TExpr, String> {
    #[allow(
        clippy::unwrap_used,
        reason = "pest grammar guarantees parse_atom's rule has an inner pair"
    )]
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::int_lit => Ok(TExpr::TInt(
            inner.as_str().parse::<i64>().map_err(|e| e.to_string())?,
        )),
        Rule::string_lit => {
            let raw = inner.into_inner().next().map(|p| p.as_str()).unwrap_or("");
            Ok(TExpr::TStr(unescape_string(raw)))
        }
        Rule::bool_lit => Ok(TExpr::TBool(inner.as_str() == "true")),
        Rule::ident => Ok(TExpr::TVar(inner.as_str().to_string())),
        Rule::list_lit => {
            let args = if let Some(arg_list) = inner.into_inner().next() {
                parse_arg_list(arg_list)?
            } else {
                vec![]
            };
            Ok(TExpr::TList(args))
        }
        Rule::expr => parse_expr(inner),
        _ => Err(format!("Unexpected atom rule: {:?}", inner.as_rule())),
    }
}

/// Process standard C-style escape sequences inside a string literal.
/// The pest grammar permits `\` followed by any char; this resolves the
/// escapes to their real characters (`\n`, `\t`, `\\`, `\"`, etc.).
fn unescape_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('0') => out.push('\0'),
                Some(other) => {
                    // Unknown escape: preserve both chars verbatim.
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn map_builtin(name: &str) -> Option<BuiltinId> {
    match name {
        "print" => Some(BuiltinId::Print),
        "fetch" => Some(BuiltinId::Fetch),
        "read_file" => Some(BuiltinId::ReadFile),
        "write_file" => Some(BuiltinId::WriteFile),
        "len" => Some(BuiltinId::Len),
        "str" => Some(BuiltinId::Str),
        "int" => Some(BuiltinId::Int),
        "concat" => Some(BuiltinId::Concat),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One table-driven case: `input` must parse to `expected`.
    struct Case {
        name: &'static str,
        input: &'static str,
        expected: TExpr,
    }

    fn c(name: &'static str, input: &'static str, expected: TExpr) -> Case {
        Case {
            name,
            input,
            expected,
        }
    }

    fn assert_all_parse(cases: Vec<Case>) {
        for case in cases {
            match parse(case.input) {
                Ok(actual) => assert_eq!(
                    actual, case.expected,
                    "{}: parse({:?}) produced an unexpected AST",
                    case.name, case.input
                ),
                Err(e) => panic!("{}: parse({:?}) failed: {e}", case.name, case.input),
            }
        }
    }

    #[test]
    fn success_literals_and_atoms() {
        assert_all_parse(vec![
            c("int", "42", TExpr::TInt(42)),
            c("string", "\"hello\"", TExpr::TStr("hello".into())),
            c("bool_true", "true", TExpr::TBool(true)),
            c("bool_false", "false", TExpr::TBool(false)),
            c("var", "x", TExpr::TVar("x".into())),
            c("zero", "0", TExpr::TInt(0)),
            c("large_int", "999999", TExpr::TInt(999999)),
            c("empty_string", r#""""#, TExpr::TStr("".into())),
            c(
                "string_escape_newline",
                r#""hello\nworld""#,
                TExpr::TStr("hello\nworld".into()),
            ),
            c(
                "string_escape_quote",
                r#""say \"hi\"""#,
                TExpr::TStr("say \"hi\"".into()),
            ),
            c(
                "string_escape_tab",
                r#""tab\there""#,
                TExpr::TStr("tab\there".into()),
            ),
            c("empty_list", "[]", TExpr::TList(vec![])),
            c(
                "list",
                "[1, 2, 3]",
                TExpr::TList(vec![TExpr::TInt(1), TExpr::TInt(2), TExpr::TInt(3)]),
            ),
            c(
                "nested_list",
                "[[1], [2]]",
                TExpr::TList(vec![
                    TExpr::TList(vec![TExpr::TInt(1)]),
                    TExpr::TList(vec![TExpr::TInt(2)]),
                ]),
            ),
            c("underscore_ident", "_foo", TExpr::TVar("_foo".into())),
            c("ident_with_digits", "x1", TExpr::TVar("x1".into())),
            c(
                "keyword_prefix_ident",
                "letters",
                TExpr::TVar("letters".into()),
            ),
            c("if_prefix_ident", "iffy", TExpr::TVar("iffy".into())),
            c(
                "extra_whitespace",
                "  1  +  2  ",
                TExpr::TBinOp(
                    BinOp::Add,
                    Box::new(TExpr::TInt(1)),
                    Box::new(TExpr::TInt(2)),
                ),
            ),
            c("with_comment", "42 -- the answer", TExpr::TInt(42)),
        ]);
    }

    #[test]
    fn success_operators() {
        assert_all_parse(vec![
            c(
                "add",
                "2 + 3",
                TExpr::TBinOp(
                    BinOp::Add,
                    Box::new(TExpr::TInt(2)),
                    Box::new(TExpr::TInt(3)),
                ),
            ),
            c(
                "sub",
                "5 - 3",
                TExpr::TBinOp(
                    BinOp::Sub,
                    Box::new(TExpr::TInt(5)),
                    Box::new(TExpr::TInt(3)),
                ),
            ),
            c(
                "mul",
                "4 * 7",
                TExpr::TBinOp(
                    BinOp::Mul,
                    Box::new(TExpr::TInt(4)),
                    Box::new(TExpr::TInt(7)),
                ),
            ),
            c(
                "div",
                "10 / 2",
                TExpr::TBinOp(
                    BinOp::Div,
                    Box::new(TExpr::TInt(10)),
                    Box::new(TExpr::TInt(2)),
                ),
            ),
            c(
                "concat",
                r#""a" ++ "b""#,
                TExpr::TBinOp(
                    BinOp::Concat,
                    Box::new(TExpr::TStr("a".into())),
                    Box::new(TExpr::TStr("b".into())),
                ),
            ),
            c(
                "eq",
                "x == 0",
                TExpr::TBinOp(
                    BinOp::Eq,
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TInt(0)),
                ),
            ),
            c(
                "ne",
                "a != b",
                TExpr::TBinOp(
                    BinOp::Ne,
                    Box::new(TExpr::TVar("a".into())),
                    Box::new(TExpr::TVar("b".into())),
                ),
            ),
            c(
                "lt",
                "x < 10",
                TExpr::TBinOp(
                    BinOp::Lt,
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TInt(10)),
                ),
            ),
            c(
                "gt",
                "x > 0",
                TExpr::TBinOp(
                    BinOp::Gt,
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TInt(0)),
                ),
            ),
            c(
                "le",
                "x <= 5",
                TExpr::TBinOp(
                    BinOp::Le,
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TInt(5)),
                ),
            ),
            c(
                "ge",
                "x >= 1",
                TExpr::TBinOp(
                    BinOp::Ge,
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TInt(1)),
                ),
            ),
            c(
                "negation",
                "-5",
                TExpr::TBinOp(
                    BinOp::Sub,
                    Box::new(TExpr::TInt(0)),
                    Box::new(TExpr::TInt(5)),
                ),
            ),
        ]);
    }

    #[test]
    fn success_precedence() {
        assert_all_parse(vec![
            c(
                "mul_binds_tighter_than_add",
                "1 + 2 * 3",
                TExpr::TBinOp(
                    BinOp::Add,
                    Box::new(TExpr::TInt(1)),
                    Box::new(TExpr::TBinOp(
                        BinOp::Mul,
                        Box::new(TExpr::TInt(2)),
                        Box::new(TExpr::TInt(3)),
                    )),
                ),
            ),
            c(
                "parens_override_precedence",
                "(1 + 2) * 3",
                TExpr::TBinOp(
                    BinOp::Mul,
                    Box::new(TExpr::TBinOp(
                        BinOp::Add,
                        Box::new(TExpr::TInt(1)),
                        Box::new(TExpr::TInt(2)),
                    )),
                    Box::new(TExpr::TInt(3)),
                ),
            ),
            c(
                "add_left_associative",
                "1 + 2 + 3",
                TExpr::TBinOp(
                    BinOp::Add,
                    Box::new(TExpr::TBinOp(
                        BinOp::Add,
                        Box::new(TExpr::TInt(1)),
                        Box::new(TExpr::TInt(2)),
                    )),
                    Box::new(TExpr::TInt(3)),
                ),
            ),
            c(
                "mul_div_left_associative",
                "6 * 2 / 3",
                TExpr::TBinOp(
                    BinOp::Div,
                    Box::new(TExpr::TBinOp(
                        BinOp::Mul,
                        Box::new(TExpr::TInt(6)),
                        Box::new(TExpr::TInt(2)),
                    )),
                    Box::new(TExpr::TInt(3)),
                ),
            ),
            c(
                "double_negation_needs_parens",
                "-(-x)",
                TExpr::TBinOp(
                    BinOp::Sub,
                    Box::new(TExpr::TInt(0)),
                    Box::new(TExpr::TBinOp(
                        BinOp::Sub,
                        Box::new(TExpr::TInt(0)),
                        Box::new(TExpr::TVar("x".into())),
                    )),
                ),
            ),
        ]);
    }

    #[test]
    fn success_let_expressions() {
        assert_all_parse(vec![
            c(
                "let_with_body",
                "let x = 5; x",
                TExpr::TLet(
                    "x".into(),
                    Box::new(TExpr::TInt(5)),
                    Box::new(TExpr::TVar("x".into())),
                ),
            ),
            c(
                "let_no_body",
                "let x = 5",
                TExpr::TBind("x".into(), Box::new(TExpr::TInt(5))),
            ),
            c(
                "let_with_binop_value",
                "let x = 1 + 2",
                TExpr::TBind(
                    "x".into(),
                    Box::new(TExpr::TBinOp(
                        BinOp::Add,
                        Box::new(TExpr::TInt(1)),
                        Box::new(TExpr::TInt(2)),
                    )),
                ),
            ),
            c(
                "nested_let",
                "let x = 1; let y = 2; x + y",
                TExpr::TLet(
                    "x".into(),
                    Box::new(TExpr::TInt(1)),
                    Box::new(TExpr::TLet(
                        "y".into(),
                        Box::new(TExpr::TInt(2)),
                        Box::new(TExpr::TBinOp(
                            BinOp::Add,
                            Box::new(TExpr::TVar("x".into())),
                            Box::new(TExpr::TVar("y".into())),
                        )),
                    )),
                ),
            ),
            c(
                "let_lambda_value",
                r#"let inc = \x -> x + 1"#,
                TExpr::TBind(
                    "inc".into(),
                    Box::new(TExpr::TLam(
                        vec!["x".into()],
                        Box::new(TExpr::TBinOp(
                            BinOp::Add,
                            Box::new(TExpr::TVar("x".into())),
                            Box::new(TExpr::TInt(1)),
                        )),
                    )),
                ),
            ),
            c(
                "let_if_value",
                "let x = if true then 1 else 2",
                TExpr::TBind(
                    "x".into(),
                    Box::new(TExpr::TIf(
                        Box::new(TExpr::TBool(true)),
                        Box::new(TExpr::TInt(1)),
                        Box::new(TExpr::TInt(2)),
                    )),
                ),
            ),
            c(
                "complex_let_and_arith",
                "let x = 2 + 3; x * 10",
                TExpr::TLet(
                    "x".into(),
                    Box::new(TExpr::TBinOp(
                        BinOp::Add,
                        Box::new(TExpr::TInt(2)),
                        Box::new(TExpr::TInt(3)),
                    )),
                    Box::new(TExpr::TBinOp(
                        BinOp::Mul,
                        Box::new(TExpr::TVar("x".into())),
                        Box::new(TExpr::TInt(10)),
                    )),
                ),
            ),
        ]);
    }

    #[test]
    fn success_if_expressions() {
        assert_all_parse(vec![
            c(
                "if_basic",
                "if true then 1 else 2",
                TExpr::TIf(
                    Box::new(TExpr::TBool(true)),
                    Box::new(TExpr::TInt(1)),
                    Box::new(TExpr::TInt(2)),
                ),
            ),
            c(
                "if_with_comparison",
                "if x > 0 then x else -x",
                TExpr::TIf(
                    Box::new(TExpr::TBinOp(
                        BinOp::Gt,
                        Box::new(TExpr::TVar("x".into())),
                        Box::new(TExpr::TInt(0)),
                    )),
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TBinOp(
                        BinOp::Sub,
                        Box::new(TExpr::TInt(0)),
                        Box::new(TExpr::TVar("x".into())),
                    )),
                ),
            ),
            c(
                "nested_if",
                "if true then if false then 1 else 2 else 3",
                TExpr::TIf(
                    Box::new(TExpr::TBool(true)),
                    Box::new(TExpr::TIf(
                        Box::new(TExpr::TBool(false)),
                        Box::new(TExpr::TInt(1)),
                        Box::new(TExpr::TInt(2)),
                    )),
                    Box::new(TExpr::TInt(3)),
                ),
            ),
        ]);
    }

    #[test]
    fn success_lambda_expressions() {
        assert_all_parse(vec![
            c(
                "single_param",
                r#"\x -> x"#,
                TExpr::TLam(vec!["x".into()], Box::new(TExpr::TVar("x".into()))),
            ),
            c(
                "multi_param",
                r#"\x y -> x + y"#,
                TExpr::TLam(
                    vec!["x".into(), "y".into()],
                    Box::new(TExpr::TBinOp(
                        BinOp::Add,
                        Box::new(TExpr::TVar("x".into())),
                        Box::new(TExpr::TVar("y".into())),
                    )),
                ),
            ),
        ]);
    }

    #[test]
    fn success_function_calls() {
        assert_all_parse(vec![
            c("call_no_args", "f()", TExpr::TVar("f".into())),
            c(
                "call_single_arg",
                "f(42)",
                TExpr::TApp(Box::new(TExpr::TVar("f".into())), vec![TExpr::TInt(42)]),
            ),
            c(
                "call_expr_arg",
                "f(1 + 2)",
                TExpr::TApp(
                    Box::new(TExpr::TVar("f".into())),
                    vec![TExpr::TBinOp(
                        BinOp::Add,
                        Box::new(TExpr::TInt(1)),
                        Box::new(TExpr::TInt(2)),
                    )],
                ),
            ),
            c(
                "call_two_args",
                "f(1, 2)",
                TExpr::TApp(
                    Box::new(TExpr::TVar("f".into())),
                    vec![TExpr::TInt(1), TExpr::TInt(2)],
                ),
            ),
            c(
                "chained_calls",
                "f(1)(2)",
                TExpr::TApp(
                    Box::new(TExpr::TApp(
                        Box::new(TExpr::TVar("f".into())),
                        vec![TExpr::TInt(1)],
                    )),
                    vec![TExpr::TInt(2)],
                ),
            ),
        ]);
    }

    #[test]
    fn success_builtins() {
        assert_all_parse(vec![
            c(
                "print",
                "print(42)",
                TExpr::TBuiltin(BuiltinId::Print, vec![TExpr::TInt(42)]),
            ),
            c(
                "write_file_multiple_args",
                r#"write_file("a.txt", "hi")"#,
                TExpr::TBuiltin(
                    BuiltinId::WriteFile,
                    vec![TExpr::TStr("a.txt".into()), TExpr::TStr("hi".into())],
                ),
            ),
        ]);
    }

    /// Every builtin name maps to its `BuiltinId` tag.
    #[test]
    fn test_parse_all_builtins() {
        let cases: [(&str, BuiltinId); 8] = [
            ("print", BuiltinId::Print),
            ("fetch", BuiltinId::Fetch),
            ("read_file", BuiltinId::ReadFile),
            ("write_file", BuiltinId::WriteFile),
            ("len", BuiltinId::Len),
            ("str", BuiltinId::Str),
            ("int", BuiltinId::Int),
            ("concat", BuiltinId::Concat),
        ];
        for (name, id) in cases {
            let input = format!("{}(1)", name);
            assert_eq!(
                parse(&input).unwrap(),
                TExpr::TBuiltin(id.clone(), vec![TExpr::TInt(1)]),
                "builtin {} should map to {:?}",
                name,
                id
            );
        }
    }

    #[test]
    fn error_cases() {
        let cases: [(&str, &str); 5] = [
            ("empty_input", ""),
            ("unclosed_paren", "(1 + 2"),
            ("unclosed_string", "\"hello"),
            ("trailing_op", "1 +"),
            ("double_negation_without_parens", "--x"),
        ];
        for (name, input) in cases {
            assert!(
                parse(input).is_err(),
                "{name}: expected parse({input:?}) to fail, but it succeeded"
            );
        }
    }
}
