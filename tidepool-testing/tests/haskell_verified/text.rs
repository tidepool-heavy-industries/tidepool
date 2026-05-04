use crate::{arb_text, run_template};
use proptest::prelude::*;
use serde_json::json;

fn arb_nat() -> impl Strategy<Value = usize> {
    0usize..=40
}

fn arb_sep() -> impl Strategy<Value = String> {
    proptest::sample::select(vec![
        ",".to_string(),
        " ".to_string(),
        "-".to_string(),
        ";".to_string(),
    ])
}

// Template 6 (text-slice)
fn gen_text_slice() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (arb_nat(), arb_text()).prop_map(|(n, s): (usize, String)| {
            let src = format!("(T.take {} {:?})", n, s);
            let expected = s.chars().take(n).collect::<String>();
            (src, json!(expected))
        }),
        (arb_nat(), arb_text()).prop_map(|(n, s): (usize, String)| {
            let src = format!("(T.drop {} {:?})", n, s);
            let expected = s.chars().skip(n).collect::<String>();
            (src, json!(expected))
        }),
        (arb_nat(), arb_text()).prop_map(|(n, s): (usize, String)| {
            let src = format!("(T.splitAt {} {:?})", n, s);
            let left = s.chars().take(n).collect::<String>();
            let right = s.chars().skip(n).collect::<String>();
            (src, json!([left, right]))
        }),
        arb_text().prop_map(|s: String| {
            let src = format!("(T.length {:?})", s);
            let expected = s.chars().count();
            (src, json!(expected))
        }),
        arb_text().prop_map(|s: String| {
            let src = format!("(T.null {:?})", s);
            let expected = s.is_empty();
            (src, json!(expected))
        })
    ]
}

#[test]
fn test_text_slice() {
    run_template(50, gen_text_slice());
}

// Template 7 (text-pred)
fn gen_text_pred() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        arb_text().prop_map(|s: String| {
            // isAlpha
            let src = format!(
                "(T.takeWhile (\\c -> (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')) {:?})",
                s
            );
            let expected = s
                .chars()
                .take_while(|c: &char| c.is_ascii_alphabetic())
                .collect::<String>();
            (src, json!(expected))
        }),
        arb_text().prop_map(|s: String| {
            // dropWhile isDigit
            let src = format!("(T.dropWhile (\\c -> c >= '0' && c <= '9') {:?})", s);
            let expected = s
                .chars()
                .skip_while(|c: &char| c.is_ascii_digit())
                .collect::<String>();
            (src, json!(expected))
        })
    ]
}

#[test]
fn test_text_pred() {
    run_template(50, gen_text_pred());
}

// Template 8 (text-case)
//
// The empty-string fast path triggers #302 (T.toUpper/T.toLower on "" yield
// UnresolvedVar). We filter empty inputs from the active template to preserve
// non-empty coverage and keep a separate, explicit regression test below for
// the empty-string case. When #302 is fixed, drop the filter and remove the
// regression test (or convert it to assert success).
fn gen_text_case() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        arb_text()
            .prop_filter("non-empty (#302)", |s| !s.is_empty())
            .prop_map(|s: String| {
                let src = format!("(T.toUpper {:?})", s);
                (src, json!(s.to_ascii_uppercase()))
            }),
        arb_text()
            .prop_filter("non-empty (#302)", |s| !s.is_empty())
            .prop_map(|s: String| {
                let src = format!("(T.toLower {:?})", s);
                (src, json!(s.to_ascii_lowercase()))
            })
    ]
}

#[test]
fn test_text_case() {
    run_template(50, gen_text_case());
}

/// Targeted regression for #302 — T.toUpper/T.toLower on empty Text yields
/// UnresolvedVar. Re-enable (and assert success) when #302 lands.
#[test]
fn test_text_case_empty_string_regression() {
    let upper = crate::compile_run_pure(r#"(T.toUpper "")"#);
    assert_eq!(upper, json!(""));
    let lower = crate::compile_run_pure(r#"(T.toLower "")"#);
    assert_eq!(lower, json!(""));
}

// Template 9 (text-split-join)
fn gen_text_split_join() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (arb_sep(), arb_text()).prop_map(|(sep, s): (String, String)| {
            let src = format!("(T.splitOn {:?} {:?})", sep, s);
            let expected: Vec<&str> = s.split(&sep).collect();
            (src, json!(expected))
        }),
        (arb_sep(), arb_text(), arb_text(), arb_text()).prop_map(
            |(sep, a, b, c): (String, String, String, String)| {
                let src = format!("(T.intercalate {:?} [{:?}, {:?}, {:?}])", sep, a, b, c);
                let expected = [a.as_str(), b.as_str(), c.as_str()].join(&sep);
                (src, json!(expected))
            }
        )
    ]
}

#[test]
fn test_text_split_join() {
    run_template(50, gen_text_split_join());
}

// Template 10 (text-strip-replace)
fn gen_text_strip_replace() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (proptest::string::string_regex(r" {0,3}[a-zA-Z0-9, \.!]{0,20} {0,3}").unwrap()).prop_map(
            |s: String| {
                let src = format!("(T.strip {:?})", s);
                let expected = s.trim();
                (src, json!(expected))
            }
        ),
        (arb_sep(), arb_sep(), arb_text()).prop_map(
            |(needle, repl, s): (String, String, String)| {
                let src = format!("(T.replace {:?} {:?} {:?})", needle, repl, s);
                let expected = s.replace(&needle, &repl);
                (src, json!(expected))
            }
        )
    ]
}

#[test]
fn test_text_strip_replace() {
    run_template(50, gen_text_strip_replace());
}

// Template 11 (text-prefix-checks)
fn gen_text_prefix_checks() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (arb_text(), arb_text()).prop_map(|(p, s): (String, String)| {
            let src = format!("(T.isPrefixOf {:?} {:?})", p, s);
            (src, json!(s.starts_with(&p)))
        }),
        (arb_text(), arb_text()).prop_map(|(p, s): (String, String)| {
            let src = format!("(T.isSuffixOf {:?} {:?})", p, s);
            (src, json!(s.ends_with(&p)))
        }),
        (arb_text(), arb_text()).prop_map(|(p, s): (String, String)| {
            let src = format!("(T.isInfixOf {:?} {:?})", p, s);
            (src, json!(s.contains(&p)))
        })
    ]
}

#[test]
fn test_text_prefix_checks() {
    run_template(50, gen_text_prefix_checks());
}
