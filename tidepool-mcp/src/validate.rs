//! Resume-response validation against the JSON-Schema subset emitted by the
//! Haskell `schemaToValue` (see `ask_decl` in lib.rs).
//!
//! The contract: a schema'd suspension (`ask`, via `AskWith` with a "schema" key)
//! requires the resume reply to match. Validation is coerce-into-contract,
//! not gatekeeping for its own sake:
//!   * a string reply that *contains* JSON is unwrapped one level when the
//!     schema expects an object/array (the #315 double-stringify failure mode);
//!   * a non-JSON raw text reply is accepted as a string value when the
//!     schema's top level is string/enum (enum membership still enforced);
//!   * the CANONICAL parsed value — not the raw text — is what crosses into
//!     the Haskell computation, so the validator and the Q parser can never
//!     disagree about what was delivered.
//!
//! The walker enforces exactly what `schemaToValue` emits: `type`
//! (object/string/number/boolean/array), `properties`, `required`, `items`,
//! `enum`. Unknown keywords are IGNORED (raw `askWith` users may embed real
//! JSON Schema keywords we don't implement); extra object keys are ALLOWED.

use serde_json::Value as Json;

/// One schema violation, path-annotated (`$.category: expected one of
/// ["bug","refactor"], got "typo"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub expected: String,
    pub got: String,
}

impl Violation {
    fn new(path: &str, expected: impl Into<String>, got: &Json) -> Self {
        Violation {
            path: path.to_string(),
            expected: expected.into(),
            got: summarize(got),
        }
    }

    pub fn to_json(&self) -> Json {
        serde_json::json!({
            "path": self.path,
            "expected": self.expected,
            "got": self.got,
        })
    }
}

/// Outcome of validating a resume response.
#[derive(Debug)]
pub enum Outcome {
    /// The canonical value to deliver to the Haskell computation.
    Valid(Json),
    /// Path-annotated violations; the continuation must NOT be consumed.
    Invalid(Vec<Violation>),
}

/// Short single-line description of a JSON value for error messages.
fn summarize(v: &Json) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 {
        let truncated: String = s.chars().take(80).collect();
        format!("{}…", truncated)
    } else {
        s
    }
}

fn schema_type(schema: &Json) -> Option<&str> {
    schema.get("type").and_then(|t| t.as_str())
}

fn is_stringy_schema(schema: &Json) -> bool {
    // SStr and SEnum both render as {"type": "string", ...}.
    schema_type(schema) == Some("string") || schema.get("enum").is_some()
}

/// Validate a resume response against an optional schema.
///
/// `response` is the raw value from the resume tool call (a JSON value;
/// plain-text callers arrive as `Json::String`).
///
/// Schema-less asks: always `Valid` — a string that parses as JSON is
/// unwrapped (today's lenient behavior), anything else passes through.
pub fn validate_response(schema: Option<&Json>, response: &Json) -> Outcome {
    let Some(schema) = schema else {
        // No schema: preserve the historical leniency — a string reply that
        // is itself valid JSON is delivered parsed; otherwise as a string.
        if let Json::String(s) = response {
            if let Ok(parsed) = serde_json::from_str::<Json>(s) {
                return Outcome::Valid(parsed);
            }
        }
        return Outcome::Valid(response.clone());
    };

    // Candidate 1: the response as given (string replies tried as parsed
    // JSON first when they contain JSON).
    let mut candidates: Vec<Json> = Vec::new();
    match response {
        Json::String(s) => {
            if let Ok(parsed) = serde_json::from_str::<Json>(s.trim()) {
                // One-level unwrap of stringified JSON (the #315 failure
                // mode) — but only adopt it when the schema wants structure
                // or it validates; raw-text fallback below covers strings.
                candidates.push(parsed);
            }
            if is_stringy_schema(schema) {
                // Raw trimmed text as the string value (covers `refactor`
                // unquoted, and `3` against SStr).
                candidates.push(Json::String(s.trim().to_string()));
            }
        }
        other => candidates.push(other.clone()),
    }

    if candidates.is_empty() {
        return Outcome::Invalid(vec![Violation {
            path: "$".into(),
            expected: schema_summary(schema),
            got: summarize(response),
        }]);
    }

    let mut first_failure: Option<Vec<Violation>> = None;
    for candidate in candidates {
        let mut violations = Vec::new();
        check(schema, &candidate, "$", &mut violations);
        if violations.is_empty() {
            return Outcome::Valid(candidate);
        }
        if first_failure.is_none() {
            first_failure = Some(violations);
        }
    }
    Outcome::Invalid(first_failure.unwrap_or_default())
}

fn schema_summary(schema: &Json) -> String {
    if let Some(e) = schema.get("enum") {
        format!("one of {}", e)
    } else if let Some(t) = schema_type(schema) {
        t.to_string()
    } else {
        summarize(schema)
    }
}

/// Recursive subset walker. Appends path-annotated violations.
fn check(schema: &Json, value: &Json, path: &str, out: &mut Vec<Violation>) {
    // enum check applies regardless of declared type
    if let Some(Json::Array(variants)) = schema.get("enum") {
        if !variants.contains(value) {
            out.push(Violation::new(
                path,
                format!("one of {}", Json::Array(variants.clone())),
                value,
            ));
            return;
        }
    }

    match schema_type(schema) {
        Some("object") => {
            let Json::Object(map) = value else {
                out.push(Violation::new(path, "object", value));
                return;
            };
            if let Some(Json::Array(required)) = schema.get("required") {
                for key in required.iter().filter_map(|k| k.as_str()) {
                    if !map.contains_key(key) {
                        out.push(Violation {
                            path: format!("{}.{}", path, key),
                            expected: "required field".into(),
                            got: "missing".into(),
                        });
                    }
                }
            }
            if let Some(Json::Object(props)) = schema.get("properties") {
                for (key, sub_schema) in props {
                    if let Some(sub_value) = map.get(key) {
                        check(sub_schema, sub_value, &format!("{}.{}", path, key), out);
                    }
                }
            }
            // extra keys: allowed
        }
        Some("array") => {
            let Json::Array(items) = value else {
                out.push(Violation::new(path, "array", value));
                return;
            };
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    check(item_schema, item, &format!("{}[{}]", path, i), out);
                }
            }
        }
        Some("string") => {
            if !value.is_string() {
                out.push(Violation::new(path, "string", value));
            }
        }
        Some("number") => {
            if !value.is_number() {
                out.push(Violation::new(path, "number", value));
            }
        }
        Some("boolean") => {
            if !value.is_boolean() {
                out.push(Violation::new(path, "boolean", value));
            }
        }
        // Unknown or absent type: best-effort subset — ignore.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid(o: Outcome) -> Json {
        match o {
            Outcome::Valid(v) => v,
            Outcome::Invalid(v) => panic!("expected Valid, got Invalid: {:?}", v),
        }
    }

    fn invalid(o: Outcome) -> Vec<Violation> {
        match o {
            Outcome::Invalid(v) => v,
            Outcome::Valid(v) => panic!("expected Invalid, got Valid: {}", v),
        }
    }

    #[test]
    fn enum_accepts_raw_text() {
        // schemaToValue (SEnum ["bug","refactor"])
        let schema = json!({"type": "string", "enum": ["bug", "refactor"]});
        let v = valid(validate_response(
            Some(&schema),
            &json!("refactor"), // raw unquoted text arrives as a JSON string
        ));
        assert_eq!(v, json!("refactor"));
    }

    #[test]
    fn enum_rejects_nonmember() {
        let schema = json!({"type": "string", "enum": ["bug", "refactor"]});
        let vs = invalid(validate_response(Some(&schema), &json!("typo")));
        assert_eq!(vs[0].path, "$");
        assert!(vs[0].expected.contains("refactor"));
    }

    #[test]
    fn sstr_accepts_raw_number_text_as_string() {
        // raw reply `3` against SStr: raw-text fallback delivers "3"
        let schema = json!({"type": "string"});
        let v = valid(validate_response(Some(&schema), &json!("3")));
        assert_eq!(v, json!("3"));
    }

    #[test]
    fn object_schema_unwraps_stringified_json() {
        // the #315 double-stringify failure mode
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "number"}},
            "required": ["a"]
        });
        let v = valid(validate_response(
            Some(&schema),
            &json!("{\"a\": 1}"), // object arrived stringified
        ));
        assert_eq!(v, json!({"a": 1}));
    }

    #[test]
    fn object_schema_rejects_prose() {
        let schema = json!({"type": "object", "properties": {}, "required": []});
        let vs = invalid(validate_response(
            Some(&schema),
            &json!("this is just prose"),
        ));
        assert_eq!(vs[0].expected, "object");
    }

    #[test]
    fn nested_path_violation() {
        let schema = json!({
            "type": "object",
            "properties": {
                "pick": {"type": "string", "enum": ["a", "b"]},
                "pri": {"type": "number"}
            },
            "required": ["pick", "pri"]
        });
        let vs = invalid(validate_response(
            Some(&schema),
            &json!({"pick": "c", "pri": "high"}),
        ));
        let paths: Vec<&str> = vs.iter().map(|v| v.path.as_str()).collect();
        assert!(paths.contains(&"$.pick"));
        assert!(paths.contains(&"$.pri"));
    }

    #[test]
    fn missing_required_field() {
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "boolean"}},
            "required": ["answer"]
        });
        let vs = invalid(validate_response(Some(&schema), &json!({})));
        assert_eq!(vs[0].path, "$.answer");
        assert_eq!(vs[0].got, "missing");
    }

    #[test]
    fn extra_keys_allowed() {
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "boolean"}},
            "required": ["answer"]
        });
        let v = valid(validate_response(
            Some(&schema),
            &json!({"answer": true, "rationale": "because"}),
        ));
        assert_eq!(v["answer"], json!(true));
    }

    #[test]
    fn unknown_keywords_ignored() {
        let schema = json!({"type": "string", "minLength": 5, "format": "email"});
        let v = valid(validate_response(Some(&schema), &json!("hi")));
        assert_eq!(v, json!("hi"));
    }

    #[test]
    fn array_items_validated() {
        let schema = json!({"type": "array", "items": {"type": "number"}});
        let vs = invalid(validate_response(Some(&schema), &json!([1, "two", 3])));
        assert_eq!(vs[0].path, "$[1]");
    }

    #[test]
    fn schemaless_parses_json_strings() {
        let v = valid(validate_response(None, &json!("{\"a\": 1}")));
        assert_eq!(v, json!({"a": 1}));
    }

    #[test]
    fn schemaless_passes_plain_text() {
        let v = valid(validate_response(None, &json!("just an answer")));
        assert_eq!(v, json!("just an answer"));
    }

    #[test]
    fn structured_response_direct() {
        // resume called with a real JSON object (not a string)
        let schema = json!({
            "type": "object",
            "properties": {"pick": {"type": "string"}},
            "required": ["pick"]
        });
        let v = valid(validate_response(Some(&schema), &json!({"pick": "bug"})));
        assert_eq!(v["pick"], json!("bug"));
    }
}
