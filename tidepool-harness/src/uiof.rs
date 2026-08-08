//! Server-derived operator forms (`uiOf`): map a typed hole's answer
//! type to a mechanical [`Ui`] form purely from the compiled artifact's
//! [`DataConTable`] — no Haskell Generic machinery, no fluency tax.
//!
//! The table captures field LABELS but not field TYPES, so per-field widget
//! choice can't inspect a field's type: every record field renders as a
//! [`Ui::TextIn`] (a safe default). [`Ui::Choice`] is reserved for the case
//! where the ANSWER TYPE ITSELF is a nullary sum (`Bool`, `data Verdict = GO
//! | PARTIAL | NOGO`, …) — every constructor has zero fields, so there is
//! nothing to widen, just a set of names to pick from.
//!
//! Anything else — a multi-constructor type with fields, a single
//! constructor with no captured field labels (positional, not record), or a
//! type name the table has no constructors for — yields `None`. The caller
//! falls back to the Code+eval hole card; a missing form beats a wrong one.

use serde_json::{Map, Value as Json};

use tidepool_repr::DataConTable;

use crate::ui::Ui;

/// Derive a mechanical `Ui` form for answer type `ty` from `table`.
///
/// - All of `ty`'s constructors have zero fields (a nullary sum, `Bool`
///   included) → [`Ui::Choice`] over the constructors, in declaration
///   (`dataConTag`) order — the order [`DataConTable::constructors_of_type`]
///   already guarantees.
/// - Exactly one constructor, and it carries field labels (a record) →
///   [`Ui::Card`] of one labeled [`Ui::TextIn`] per field, in field order.
/// - Anything else → `None`.
#[must_use]
pub fn ui_of(table: &DataConTable, ty: &str) -> Option<Ui> {
    let ids = table.constructors_of_type(ty);
    if ids.is_empty() {
        return None;
    }

    let all_nullary = ids
        .iter()
        .all(|&id| table.get(id).is_some_and(|dc| dc.rep_arity == 0));
    if all_nullary {
        let options: Vec<(String, String)> = ids
            .iter()
            .filter_map(|&id| table.name_of(id).map(|n| (n.to_string(), n.to_string())))
            .collect();
        if options.len() != ids.len() {
            // A constructor id from constructors_of_type failed to resolve a
            // name — an inconsistent table. Don't guess; no form.
            return None;
        }
        return Some(Ui::Choice {
            prompt: format!("Choose a {ty}"),
            options,
            key: None,
            selected: None,
        });
    }

    if let [id] = ids[..] {
        let labels = table.field_labels_of(id)?;
        if labels.is_empty() {
            return None;
        }
        let body = labels
            .iter()
            .map(|label| Ui::TextIn {
                prompt: label.clone(),
                multiline: false,
                key: None,
                initial: None,
            })
            .collect();
        return Some(Ui::Card {
            title: ty.to_string(),
            body,
        });
    }

    None
}

/// The module a type is defined in, derived from the module-qualified name
/// of one of its constructors (`"Verdict.NOGO"` → `"Verdict"`) — real
/// metadata the extract already captured, not a guess from the type name.
/// `None` when the type has no constructors in `table`, or none of them
/// carry a qualified name.
#[must_use]
pub fn defining_module(table: &DataConTable, ty: &str) -> Option<String> {
    table
        .constructors_of_type(ty)
        .into_iter()
        .find_map(|id| table.get(id)?.qualified_name.as_deref())
        .and_then(|qn| qn.rsplit_once('.'))
        .map(|(module, _ctor)| module.to_string())
}

/// Escape a string as a Haskell string literal (surrounding quotes
/// included). Only the escapes a mechanical, literal-only construction needs:
/// backslash, double quote, and the common whitespace escapes.
fn haskell_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A JSON scalar as a Haskell literal — `Text` becomes an escaped string
/// literal, a `Number` is rendered verbatim. Anything else (bool, array,
/// object, null) is "unparseable" here — `None`, never a guessed literal.
fn haskell_literal(v: &Json) -> Option<String> {
    match v {
        Json::String(s) => Some(haskell_string_literal(s)),
        Json::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Mechanically build the `resume` expression from a form submission's
/// `values` map (F1's `{values, prose}` answer encoding — `prose` is not
/// consulted here; a non-empty prose answer is the elaboration path's job,
/// not this one), against the `Ui` this hole's answer type derived to.
///
/// - [`Ui::Choice`]: `values` must carry EXACTLY one key, and it must name
///   one of the offered constructors — the bare constructor name is the
///   resume expression (`resume NOGO`).
/// - [`Ui::Card`] (uiOf's record shape — a flat list of labeled `TextIn`s):
///   `values` must carry EXACTLY the form's field labels, each mapping to a
///   literal-safe scalar — `TypeName { label = lit, … }` record syntax is
///   the resume expression.
/// - Any missing/extra/unparseable field, or a `Ui` shape this function
///   doesn't recognize (e.g. hand-authored `dialogAsk` UI, not a uiOf
///   output): `None` — reject rather than guess; the caller routes to the
///   prose/elaboration fallback.
#[must_use]
pub fn resume_expr_from_submission(ui: &Ui, values: &Map<String, Json>) -> Option<String> {
    match ui {
        Ui::Choice { options, .. } => {
            if values.len() != 1 {
                return None;
            }
            let (key, _) = values.iter().next().expect("len == 1 checked above");
            options
                .iter()
                .any(|(option_key, _)| option_key == key)
                .then(|| key.clone())
        }
        Ui::Card { title, body } => {
            let mut labels = Vec::with_capacity(body.len());
            for item in body {
                match item {
                    Ui::TextIn { prompt, .. } => labels.push(prompt.as_str()),
                    _ => return None,
                }
            }
            if labels.is_empty() || labels.len() != values.len() {
                return None;
            }
            let mut fields = Vec::with_capacity(labels.len());
            for label in &labels {
                let value = values.get(*label)?;
                let literal = haskell_literal(value)?;
                fields.push(format!("{label} = {literal}"));
            }
            Some(format!("{title} {{ {} }}", fields.join(", ")))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tidepool_repr::{DataCon, DataConId};

    fn nullary(id: u64, name: &str, tag: u32, type_name: &str) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some(format!("{type_name}.{name}")),
            type_name: type_name.to_string(),
        }
    }

    fn with_fields(id: u64, name: &str, tag: u32, rep_arity: u32, type_name: &str) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity,
            field_bangs: vec![],
            qualified_name: Some(format!("{type_name}.{name}")),
            type_name: type_name.to_string(),
        }
    }

    fn values(v: Json) -> Map<String, Json> {
        v.as_object().unwrap().clone()
    }

    // ---- ui_of -----------------------------------------------------------

    #[test]
    fn nullary_sum_derives_choice_in_declaration_order() {
        let mut table = DataConTable::new();
        // Insert out of tag order — constructors_of_type re-sorts by tag.
        table.insert(nullary(3, "NOGO", 3, "Verdict"));
        table.insert(nullary(1, "GO", 1, "Verdict"));
        table.insert(nullary(2, "PARTIAL", 2, "Verdict"));

        let ui = ui_of(&table, "Verdict").expect("nullary sum derives a form");
        assert_eq!(
            ui,
            Ui::Choice {
                prompt: "Choose a Verdict".to_string(),
                options: vec![
                    ("GO".to_string(), "GO".to_string()),
                    ("PARTIAL".to_string(), "PARTIAL".to_string()),
                    ("NOGO".to_string(), "NOGO".to_string()),
                ],
                key: None,
                selected: None,
            }
        );
    }

    #[test]
    fn bool_style_two_way_sum_derives_choice() {
        let mut table = DataConTable::new();
        table.insert(nullary(1, "False", 1, "Bool"));
        table.insert(nullary(2, "True", 2, "Bool"));

        let ui = ui_of(&table, "Bool").expect("2-way nullary sum derives a form");
        assert_eq!(
            ui,
            Ui::Choice {
                prompt: "Choose a Bool".to_string(),
                options: vec![
                    ("False".to_string(), "False".to_string()),
                    ("True".to_string(), "True".to_string()),
                ],
                key: None,
                selected: None,
            }
        );
    }

    #[test]
    fn single_constructor_record_derives_labeled_text_ins() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Person", 1, 2, "Person");
        table.insert(dc.clone());
        table.set_field_labels(dc.id, vec!["name".to_string(), "age".to_string()]);

        let ui = ui_of(&table, "Person").expect("single-constructor record derives a form");
        assert_eq!(
            ui,
            Ui::Card {
                title: "Person".to_string(),
                body: vec![
                    Ui::TextIn {
                        prompt: "name".to_string(),
                        multiline: false,
                        key: None,
                        initial: None,
                    },
                    Ui::TextIn {
                        prompt: "age".to_string(),
                        multiline: false,
                        key: None,
                        initial: None,
                    },
                ],
            }
        );
    }

    #[test]
    fn single_constructor_positional_no_labels_yields_none() {
        let mut table = DataConTable::new();
        // rep_arity > 0 but no field_labels recorded (positional, not record).
        table.insert(with_fields(1, "Pair", 1, 2, "Pair"));
        assert_eq!(ui_of(&table, "Pair"), None);
    }

    #[test]
    fn multi_constructor_with_fields_yields_none() {
        let mut table = DataConTable::new();
        table.insert(nullary(1, "Nothing", 1, "Maybe"));
        table.insert(with_fields(2, "Just", 2, 1, "Maybe"));
        assert_eq!(ui_of(&table, "Maybe"), None);
    }

    #[test]
    fn unknown_type_name_yields_none() {
        let table = DataConTable::new();
        assert_eq!(ui_of(&table, "NoSuchType"), None);
    }

    // ---- defining_module ---------------------------------------------------

    #[test]
    fn defining_module_derives_from_qualified_name() {
        let mut table = DataConTable::new();
        table.insert(nullary(1, "GO", 1, "Verdict"));
        table.insert(nullary(2, "NOGO", 2, "Verdict"));
        assert_eq!(
            defining_module(&table, "Verdict"),
            Some("Verdict".to_string())
        );
        assert_eq!(defining_module(&table, "NoSuchType"), None);
    }

    // ---- resume_expr_from_submission: Choice --------------------------------

    #[test]
    fn choice_submission_happy_path_yields_bare_constructor() {
        let ui = Ui::Choice {
            prompt: "Choose a Verdict".to_string(),
            options: vec![
                ("GO".to_string(), "GO".to_string()),
                ("NOGO".to_string(), "NOGO".to_string()),
            ],
            key: None,
            selected: None,
        };
        let submitted = values(json!({ "NOGO": true }));
        assert_eq!(
            resume_expr_from_submission(&ui, &submitted),
            Some("NOGO".to_string())
        );
    }

    #[test]
    fn choice_submission_unknown_key_rejects() {
        let ui = Ui::Choice {
            prompt: "Choose a Verdict".to_string(),
            options: vec![("GO".to_string(), "GO".to_string())],
            key: None,
            selected: None,
        };
        let submitted = values(json!({ "MAYBE": true }));
        assert_eq!(resume_expr_from_submission(&ui, &submitted), None);
    }

    #[test]
    fn choice_submission_multiple_keys_rejects() {
        let ui = Ui::Choice {
            prompt: "Choose a Verdict".to_string(),
            options: vec![
                ("GO".to_string(), "GO".to_string()),
                ("NOGO".to_string(), "NOGO".to_string()),
            ],
            key: None,
            selected: None,
        };
        let submitted = values(json!({ "GO": true, "NOGO": true }));
        assert_eq!(resume_expr_from_submission(&ui, &submitted), None);
    }

    #[test]
    fn choice_submission_empty_rejects() {
        let ui = Ui::Choice {
            prompt: "Choose a Verdict".to_string(),
            options: vec![("GO".to_string(), "GO".to_string())],
            key: None,
            selected: None,
        };
        let submitted = values(json!({}));
        assert_eq!(resume_expr_from_submission(&ui, &submitted), None);
    }

    // ---- resume_expr_from_submission: Card (record) -------------------------

    fn person_ui() -> Ui {
        Ui::Card {
            title: "Person".to_string(),
            body: vec![
                Ui::TextIn {
                    prompt: "name".to_string(),
                    multiline: false,
                    key: None,
                    initial: None,
                },
                Ui::TextIn {
                    prompt: "age".to_string(),
                    multiline: false,
                    key: None,
                    initial: None,
                },
            ],
        }
    }

    #[test]
    fn record_submission_happy_path_builds_record_syntax() {
        let submitted = values(json!({ "name": "Ada", "age": 36 }));
        assert_eq!(
            resume_expr_from_submission(&person_ui(), &submitted),
            Some(r#"Person { name = "Ada", age = 36 }"#.to_string())
        );
    }

    #[test]
    fn record_submission_escapes_string_literal() {
        let submitted = values(json!({ "name": "Ada \"Countess\" \\ Lovelace", "age": 36 }));
        assert_eq!(
            resume_expr_from_submission(&person_ui(), &submitted),
            Some(r#"Person { name = "Ada \"Countess\" \\ Lovelace", age = 36 }"#.to_string())
        );
    }

    #[test]
    fn record_submission_missing_field_rejects() {
        let submitted = values(json!({ "name": "Ada" }));
        assert_eq!(resume_expr_from_submission(&person_ui(), &submitted), None);
    }

    #[test]
    fn record_submission_extra_field_rejects() {
        let submitted = values(json!({ "name": "Ada", "age": 36, "extra": "nope" }));
        assert_eq!(resume_expr_from_submission(&person_ui(), &submitted), None);
    }

    #[test]
    fn record_submission_unparseable_field_rejects() {
        let submitted = values(json!({ "name": "Ada", "age": true }));
        assert_eq!(resume_expr_from_submission(&person_ui(), &submitted), None);
    }
}
