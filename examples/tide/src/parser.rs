use crate::ast::TExpr;
use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "tide.pest"]
pub struct TideParser;

pub fn parse(input: &str) -> Result<TExpr, String> {
    let pairs = TideParser::parse(Rule::program, input)
        .map_err(|e| format!("Parse error: {}", e))?;
    
    let pair = pairs.into_iter().next().unwrap();
    parse_expr(pair.into_inner().next().unwrap())
}

fn parse_expr(pair: Pair<Rule>) -> Result<TExpr, String> {
    match pair.as_rule() {
        Rule::let_expr => parse_let(pair),
        Rule::if_expr => parse_if(pair),
        Rule::lambda_expr => parse_lambda(pair),
        Rule::comparison => parse_comparison(pair),
        Rule::expr => parse_expr(pair.into_inner().next().unwrap()),
        _ => Err(format!("Unexpected rule in parse_expr: {:?}", pair.as_rule())),
    }
}

fn parse_let(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    let ident = inner.next().unwrap().as_str().to_string();
    let val = parse_expr(inner.next().unwrap())?;
    let body = if let Some(body_pair) = inner.next() {
        parse_expr(body_pair)?
    } else {
        // REPL shorthand: `let x = 5` → `let x = 5; x`
        TExpr::TVar(ident.clone())
    };
    Ok(TExpr::TLet(ident, Box::new(val), Box::new(body)))
}

fn parse_if(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    let cond = parse_expr(inner.next().unwrap())?;
    let t = parse_expr(inner.next().unwrap())?;
    let e = parse_expr(inner.next().unwrap())?;
    Ok(TExpr::TIf(Box::new(cond), Box::new(t), Box::new(e)))
}

fn parse_lambda(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner().collect::<Vec<_>>();
    let body_pair = inner.pop().unwrap();
    let params = inner.into_iter().map(|p| p.as_str().to_string()).collect();
    let body = parse_expr(body_pair)?;
    Ok(TExpr::TLam(params, Box::new(body)))
}

fn parse_comparison(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_concat, Rule::comp_op, map_comp_op)
}

fn parse_concat(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_addition, Rule::concat_op, |_| 10)
}

fn parse_addition(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_multiplication, Rule::add_op, |s| {
        if s == "+" { 0 } else { 1 }
    })
}

fn parse_multiplication(pair: Pair<Rule>) -> Result<TExpr, String> {
    parse_binary_op(pair, parse_unary, Rule::mul_op, |s| {
        if s == "*" { 2 } else { 3 }
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
    M: Fn(&str) -> i64,
{
    let mut inner = pair.into_inner();
    let first = inner.next().ok_or_else(|| "Missing left operand".to_string())?;
    let mut left = next(first)?;

    while let Some(op_pair) = inner.next() {
        let op_str = if op_pair.as_rule() == op_rule {
            op_pair.as_str()
        } else {
            // Some rules like comp_op have nested ops
            op_pair.into_inner().next().unwrap().as_str()
        };
        let op_id = mapper(op_str);
        let right = next(inner.next().unwrap())?;
        left = TExpr::TBinOp(op_id, Box::new(left), Box::new(right));
    }

    Ok(left)
}

fn map_comp_op(op: &str) -> i64 {
    match op {
        "==" => 4,
        "!=" => 5,
        "<" => 6,
        ">" => 7,
        "<=" => 8,
        ">=" => 9,
        _ => 0, // Should not happen with valid grammar
    }
}

fn parse_unary(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    let first = inner.next().unwrap();
    if first.as_rule() == Rule::neg_op {
        let val = parse_unary(inner.next().unwrap())?;
        Ok(TExpr::TBinOp(1, Box::new(TExpr::TInt(0)), Box::new(val)))
    } else {
        parse_call(first)
    }
}

fn parse_call(pair: Pair<Rule>) -> Result<TExpr, String> {
    let mut inner = pair.into_inner();
    let atom_pair = inner.next().unwrap();
    let mut current = parse_atom(atom_pair)?;

    while let Some(args_pair) = inner.next() {
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
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::int_lit => Ok(TExpr::TInt(inner.as_str().parse::<i64>().map_err(|e| e.to_string())?)),
        Rule::string_lit => {
            let s = inner.into_inner().next().map(|p| p.as_str()).unwrap_or("");
            Ok(TExpr::TStr(s.to_string()))
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

fn map_builtin(name: &str) -> Option<i64> {
    match name {
        "print" => Some(0),
        "fetch" => Some(1),
        "read_file" => Some(2),
        "write_file" => Some(3),
        "len" => Some(4),
        "str" => Some(5),
        "int" => Some(6),
        "concat" => Some(7),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_int() {
        assert_eq!(parse("42").unwrap(), TExpr::TInt(42));
    }

    #[test]
    fn test_parse_string() {
        assert_eq!(parse("\"hello\"").unwrap(), TExpr::TStr("hello".into()));
    }

    #[test]
    fn test_parse_bool() {
        assert_eq!(parse("true").unwrap(), TExpr::TBool(true));
        assert_eq!(parse("false").unwrap(), TExpr::TBool(false));
    }

    #[test]
    fn test_parse_var() {
        assert_eq!(parse("x").unwrap(), TExpr::TVar("x".into()));
    }

    #[test]
    fn test_parse_arithmetic() {
        // 2 + 3
        let expr = parse("2 + 3").unwrap();
        assert_eq!(expr, TExpr::TBinOp(0, Box::new(TExpr::TInt(2)), Box::new(TExpr::TInt(3))));
    }

    #[test]
    fn test_parse_precedence() {
        // 1 + 2 * 3 -> 1 + (2 * 3)
        let expr = parse("1 + 2 * 3").unwrap();
        assert_eq!(expr, TExpr::TBinOp(0,
            Box::new(TExpr::TInt(1)),
            Box::new(TExpr::TBinOp(2, Box::new(TExpr::TInt(2)), Box::new(TExpr::TInt(3))))
        ));
    }

    #[test]
    fn test_parse_let_with_body() {
        // let x = 5; x
        let expr = parse("let x = 5; x").unwrap();
        assert_eq!(expr, TExpr::TLet(
            "x".into(),
            Box::new(TExpr::TInt(5)),
            Box::new(TExpr::TVar("x".into()))
        ));
    }

    #[test]
    fn test_parse_let_no_body() {
        // let x = 5  (REPL shorthand, body defaults to TVar("x"))
        let expr = parse("let x = 5").unwrap();
        assert_eq!(expr, TExpr::TLet(
            "x".into(),
            Box::new(TExpr::TInt(5)),
            Box::new(TExpr::TVar("x".into()))
        ));
    }

    #[test]
    fn test_parse_if() {
        let expr = parse("if true then 1 else 2").unwrap();
        assert_eq!(expr, TExpr::TIf(
            Box::new(TExpr::TBool(true)),
            Box::new(TExpr::TInt(1)),
            Box::new(TExpr::TInt(2))
        ));
    }

    #[test]
    fn test_parse_lambda() {
        let expr = parse(r#"\x -> x"#).unwrap();
        assert_eq!(expr, TExpr::TLam(
            vec!["x".into()],
            Box::new(TExpr::TVar("x".into()))
        ));
    }

    #[test]
    fn test_parse_call() {
        let expr = parse("f(1, 2)").unwrap();
        assert_eq!(expr, TExpr::TApp(
            Box::new(TExpr::TVar("f".into())),
            vec![TExpr::TInt(1), TExpr::TInt(2)]
        ));
    }

    #[test]
    fn test_parse_builtin() {
        let expr = parse("print(42)").unwrap();
        assert_eq!(expr, TExpr::TBuiltin(0, vec![TExpr::TInt(42)]));
    }

    #[test]
    fn test_parse_list() {
        let expr = parse("[1, 2, 3]").unwrap();
        assert_eq!(expr, TExpr::TList(vec![TExpr::TInt(1), TExpr::TInt(2), TExpr::TInt(3)]));
    }

    #[test]
    fn test_parse_concat() {
        let expr = parse(r#""a" ++ "b""#).unwrap();
        assert_eq!(expr, TExpr::TBinOp(10,
            Box::new(TExpr::TStr("a".into())),
            Box::new(TExpr::TStr("b".into()))
        ));
    }

    #[test]
    fn test_parse_comparison() {
        let expr = parse("x == 0").unwrap();
        assert_eq!(expr, TExpr::TBinOp(4,
            Box::new(TExpr::TVar("x".into())),
            Box::new(TExpr::TInt(0))
        ));
    }

    #[test]
    fn test_parse_complex() {
        // let x = 2 + 3; x * 10
        let expr = parse("let x = 2 + 3; x * 10").unwrap();
        assert_eq!(expr, TExpr::TLet(
            "x".into(),
            Box::new(TExpr::TBinOp(0, Box::new(TExpr::TInt(2)), Box::new(TExpr::TInt(3)))),
            Box::new(TExpr::TBinOp(2, Box::new(TExpr::TVar("x".into())), Box::new(TExpr::TInt(10))))
        ));
    }

    // === Atoms ===

    #[test]
    fn test_parse_zero() {
        assert_eq!(parse("0").unwrap(), TExpr::TInt(0));
    }

    #[test]
    fn test_parse_large_int() {
        assert_eq!(parse("999999").unwrap(), TExpr::TInt(999999));
    }

    #[test]
    fn test_parse_empty_string() {
        assert_eq!(parse(r#""""#).unwrap(), TExpr::TStr("".into()));
    }

    #[test]
    fn test_parse_string_with_escape() {
        assert_eq!(parse(r#""hello\nworld""#).unwrap(), TExpr::TStr("hello\\nworld".into()));
    }

    #[test]
    fn test_parse_empty_list() {
        assert_eq!(parse("[]").unwrap(), TExpr::TList(vec![]));
    }

    #[test]
    fn test_parse_nested_list() {
        assert_eq!(parse("[[1], [2]]").unwrap(), TExpr::TList(vec![
            TExpr::TList(vec![TExpr::TInt(1)]),
            TExpr::TList(vec![TExpr::TInt(2)]),
        ]));
    }

    #[test]
    fn test_parse_paren_expr() {
        assert_eq!(parse("(1 + 2) * 3").unwrap(), TExpr::TBinOp(2,
            Box::new(TExpr::TBinOp(0, Box::new(TExpr::TInt(1)), Box::new(TExpr::TInt(2)))),
            Box::new(TExpr::TInt(3)),
        ));
    }

    // === Arithmetic operators ===

    #[test]
    fn test_parse_subtraction() {
        assert_eq!(parse("5 - 3").unwrap(), TExpr::TBinOp(1,
            Box::new(TExpr::TInt(5)), Box::new(TExpr::TInt(3))));
    }

    #[test]
    fn test_parse_multiplication() {
        assert_eq!(parse("4 * 7").unwrap(), TExpr::TBinOp(2,
            Box::new(TExpr::TInt(4)), Box::new(TExpr::TInt(7))));
    }

    #[test]
    fn test_parse_division() {
        assert_eq!(parse("10 / 2").unwrap(), TExpr::TBinOp(3,
            Box::new(TExpr::TInt(10)), Box::new(TExpr::TInt(2))));
    }

    #[test]
    fn test_parse_chained_addition() {
        // 1 + 2 + 3 -> (1 + 2) + 3 (left-associative)
        assert_eq!(parse("1 + 2 + 3").unwrap(), TExpr::TBinOp(0,
            Box::new(TExpr::TBinOp(0,
                Box::new(TExpr::TInt(1)), Box::new(TExpr::TInt(2)))),
            Box::new(TExpr::TInt(3)),
        ));
    }

    #[test]
    fn test_parse_mixed_mul_div() {
        // 6 * 2 / 3 -> (6 * 2) / 3
        assert_eq!(parse("6 * 2 / 3").unwrap(), TExpr::TBinOp(3,
            Box::new(TExpr::TBinOp(2,
                Box::new(TExpr::TInt(6)), Box::new(TExpr::TInt(2)))),
            Box::new(TExpr::TInt(3)),
        ));
    }

    // === Comparison operators ===

    #[test]
    fn test_parse_not_equal() {
        assert_eq!(parse("a != b").unwrap(), TExpr::TBinOp(5,
            Box::new(TExpr::TVar("a".into())), Box::new(TExpr::TVar("b".into()))));
    }

    #[test]
    fn test_parse_less_than() {
        assert_eq!(parse("x < 10").unwrap(), TExpr::TBinOp(6,
            Box::new(TExpr::TVar("x".into())), Box::new(TExpr::TInt(10))));
    }

    #[test]
    fn test_parse_greater_than() {
        assert_eq!(parse("x > 0").unwrap(), TExpr::TBinOp(7,
            Box::new(TExpr::TVar("x".into())), Box::new(TExpr::TInt(0))));
    }

    #[test]
    fn test_parse_less_equal() {
        assert_eq!(parse("x <= 5").unwrap(), TExpr::TBinOp(8,
            Box::new(TExpr::TVar("x".into())), Box::new(TExpr::TInt(5))));
    }

    #[test]
    fn test_parse_greater_equal() {
        assert_eq!(parse("x >= 1").unwrap(), TExpr::TBinOp(9,
            Box::new(TExpr::TVar("x".into())), Box::new(TExpr::TInt(1))));
    }

    // === Unary negation ===

    #[test]
    fn test_parse_negation() {
        // -5 desugars to 0 - 5
        assert_eq!(parse("-5").unwrap(), TExpr::TBinOp(1,
            Box::new(TExpr::TInt(0)), Box::new(TExpr::TInt(5))));
    }

    #[test]
    fn test_parse_double_negation_needs_parens() {
        // --x doesn't parse (neg_op consumes one `-`, second `-` isn't an expr start)
        assert!(parse("--x").is_err());
        // but -(-x) does
        assert_eq!(parse("-(-x)").unwrap(), TExpr::TBinOp(1,
            Box::new(TExpr::TInt(0)),
            Box::new(TExpr::TBinOp(1,
                Box::new(TExpr::TInt(0)), Box::new(TExpr::TVar("x".into()))))));
    }

    // === Let expressions ===

    #[test]
    fn test_parse_let_with_binop_value() {
        assert_eq!(parse("let x = 1 + 2").unwrap(), TExpr::TLet(
            "x".into(),
            Box::new(TExpr::TBinOp(0, Box::new(TExpr::TInt(1)), Box::new(TExpr::TInt(2)))),
            Box::new(TExpr::TVar("x".into())),
        ));
    }

    #[test]
    fn test_parse_nested_let() {
        assert_eq!(parse("let x = 1; let y = 2; x + y").unwrap(), TExpr::TLet(
            "x".into(),
            Box::new(TExpr::TInt(1)),
            Box::new(TExpr::TLet(
                "y".into(),
                Box::new(TExpr::TInt(2)),
                Box::new(TExpr::TBinOp(0,
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TVar("y".into())))),
            )),
        ));
    }

    #[test]
    fn test_parse_let_lambda_value() {
        // let inc = \x -> x + 1
        assert_eq!(parse(r#"let inc = \x -> x + 1"#).unwrap(), TExpr::TLet(
            "inc".into(),
            Box::new(TExpr::TLam(
                vec!["x".into()],
                Box::new(TExpr::TBinOp(0,
                    Box::new(TExpr::TVar("x".into())),
                    Box::new(TExpr::TInt(1)))))),
            Box::new(TExpr::TVar("inc".into())),
        ));
    }

    #[test]
    fn test_parse_let_if_value() {
        assert_eq!(parse("let x = if true then 1 else 2").unwrap(), TExpr::TLet(
            "x".into(),
            Box::new(TExpr::TIf(
                Box::new(TExpr::TBool(true)),
                Box::new(TExpr::TInt(1)),
                Box::new(TExpr::TInt(2)))),
            Box::new(TExpr::TVar("x".into())),
        ));
    }

    // === If expressions ===

    #[test]
    fn test_parse_if_with_comparison() {
        assert_eq!(parse("if x > 0 then x else -x").unwrap(), TExpr::TIf(
            Box::new(TExpr::TBinOp(7, Box::new(TExpr::TVar("x".into())), Box::new(TExpr::TInt(0)))),
            Box::new(TExpr::TVar("x".into())),
            Box::new(TExpr::TBinOp(1, Box::new(TExpr::TInt(0)), Box::new(TExpr::TVar("x".into())))),
        ));
    }

    #[test]
    fn test_parse_nested_if() {
        assert_eq!(parse("if true then if false then 1 else 2 else 3").unwrap(), TExpr::TIf(
            Box::new(TExpr::TBool(true)),
            Box::new(TExpr::TIf(
                Box::new(TExpr::TBool(false)),
                Box::new(TExpr::TInt(1)),
                Box::new(TExpr::TInt(2)),
            )),
            Box::new(TExpr::TInt(3)),
        ));
    }

    // === Lambda expressions ===

    #[test]
    fn test_parse_multi_param_lambda() {
        assert_eq!(parse(r#"\x y -> x + y"#).unwrap(), TExpr::TLam(
            vec!["x".into(), "y".into()],
            Box::new(TExpr::TBinOp(0,
                Box::new(TExpr::TVar("x".into())),
                Box::new(TExpr::TVar("y".into())))),
        ));
    }

    // === Function calls ===

    #[test]
    fn test_parse_call_no_args() {
        // f() with no args parses as just the identifier (arg_list is optional)
        assert_eq!(parse("f()").unwrap(), TExpr::TVar("f".into()));
    }

    #[test]
    fn test_parse_call_single_arg() {
        assert_eq!(parse("f(42)").unwrap(), TExpr::TApp(
            Box::new(TExpr::TVar("f".into())), vec![TExpr::TInt(42)]));
    }

    #[test]
    fn test_parse_call_expr_arg() {
        assert_eq!(parse("f(1 + 2)").unwrap(), TExpr::TApp(
            Box::new(TExpr::TVar("f".into())),
            vec![TExpr::TBinOp(0, Box::new(TExpr::TInt(1)), Box::new(TExpr::TInt(2)))]));
    }

    #[test]
    fn test_parse_chained_calls() {
        // f(1)(2) -> TApp(TApp(f, [1]), [2])
        assert_eq!(parse("f(1)(2)").unwrap(), TExpr::TApp(
            Box::new(TExpr::TApp(
                Box::new(TExpr::TVar("f".into())), vec![TExpr::TInt(1)])),
            vec![TExpr::TInt(2)]));
    }

    // === Builtins ===

    #[test]
    fn test_parse_all_builtins() {
        let cases = [
            ("print", 0), ("fetch", 1), ("read_file", 2), ("write_file", 3),
            ("len", 4), ("str", 5), ("int", 6), ("concat", 7),
        ];
        for (name, id) in cases {
            let input = format!("{}(1)", name);
            assert_eq!(parse(&input).unwrap(), TExpr::TBuiltin(id, vec![TExpr::TInt(1)]),
                "builtin {} should map to id {}", name, id);
        }
    }

    #[test]
    fn test_parse_builtin_multiple_args() {
        assert_eq!(parse(r#"write_file("a.txt", "hi")"#).unwrap(),
            TExpr::TBuiltin(3, vec![TExpr::TStr("a.txt".into()), TExpr::TStr("hi".into())]));
    }

    // === Identifiers and keywords ===

    #[test]
    fn test_parse_underscore_ident() {
        assert_eq!(parse("_foo").unwrap(), TExpr::TVar("_foo".into()));
    }

    #[test]
    fn test_parse_ident_with_digits() {
        assert_eq!(parse("x1").unwrap(), TExpr::TVar("x1".into()));
    }

    #[test]
    fn test_parse_keyword_prefix_ident() {
        // "letters" starts with "let" but isn't the keyword
        assert_eq!(parse("letters").unwrap(), TExpr::TVar("letters".into()));
    }

    #[test]
    fn test_parse_if_prefix_ident() {
        assert_eq!(parse("iffy").unwrap(), TExpr::TVar("iffy".into()));
    }

    // === Whitespace and comments ===

    #[test]
    fn test_parse_extra_whitespace() {
        assert_eq!(parse("  1  +  2  ").unwrap(), TExpr::TBinOp(0,
            Box::new(TExpr::TInt(1)), Box::new(TExpr::TInt(2))));
    }

    #[test]
    fn test_parse_with_comment() {
        assert_eq!(parse("42 -- the answer").unwrap(), TExpr::TInt(42));
    }

    // === Error cases ===

    #[test]
    fn test_parse_empty_fails() {
        assert!(parse("").is_err());
    }

    #[test]
    fn test_parse_unclosed_paren() {
        assert!(parse("(1 + 2").is_err());
    }

    #[test]
    fn test_parse_unclosed_string() {
        assert!(parse(r#""hello"#).is_err());
    }

    #[test]
    fn test_parse_trailing_op() {
        assert!(parse("1 +").is_err());
    }
}