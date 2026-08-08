//! The `Ui` eDSL wire mirror — the contract between the Haskell side
//! (`haskell/lib/Tidepool/Ui.hs`, whose hand-written ToJSON must emit
//! EXACTLY this serde shape) and the tidepool-web renderer. Haskell
//! speaks elicitation semantics; Datastar/HTML vocabulary exists only in
//! the renderer. R0 is first-order (no monadic sequencing — that grows in
//! R1 with the Dialog effect).
//!
//! Every `Choice` elicitation renders with an
//! open-prose escape path IN ADDITION to its options (open sum) — the
//! renderer adds it unconditionally; it is not represented (and not
//! omittable) here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ui", rename_all = "snake_case")]
pub enum Ui {
    Card {
        title: String,
        body: Vec<Ui>,
    },
    /// Markdown.
    Prose {
        text: String,
    },
    /// Fenced source block — type signatures, decls, eval sources.
    Code {
        lang: String,
        source: String,
    },
    Choice {
        prompt: String,
        /// (option-key, label) pairs; the answer references the option-key.
        options: Vec<(String, String)>,
        /// FORM FIELD key. When `Some`, this Choice is a field of an enclosing
        /// form: it renders as a radio group whose selection is submitted under
        /// `values.<key>` (not an immediate-post button set). When `None` (the
        /// default, omitted on the wire), it's a standalone one-click choice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// The option-key pre-selected as the radio group's default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<String>,
    },
    TextIn {
        prompt: String,
        multiline: bool,
        /// FORM FIELD key. When `Some`, this input is a field of an enclosing
        /// form, submitted under `values.<key>`; when `None` (default, omitted
        /// on the wire), it's a standalone single-value text box.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// The input's starting text — a draft the operator edits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial: Option<String>,
    },
    /// Checkboxes over (option-key, label) pairs — a FORM-FIELD widget, keyed
    /// exactly like a keyed `Choice`: submitted under `values.<key>` as an
    /// ARRAY of the checked option-keys.
    MultiChoice {
        prompt: String,
        options: Vec<(String, String)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Badge {
        label: String,
        kind: BadgeKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeKind {
    EffectRow,
    Fan,
    Price,
    State,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape is a CROSS-LANGUAGE contract — this test is the
    /// canonical rendering the Haskell ToJSON instances must match
    /// byte-for-byte (module key order, which serde emits in field order).
    #[test]
    fn wire_shape_is_stable() {
        let ui = Ui::Card {
            title: "hole".into(),
            body: vec![
                Ui::Code {
                    lang: "haskell".into(),
                    source: "resume :: Verdict -> M ()".into(),
                },
                Ui::Choice {
                    prompt: "verdict?".into(),
                    options: vec![("approve".into(), "Approve".into())],
                    key: None,
                    selected: None,
                },
                Ui::Badge {
                    label: "Exec, Fs".into(),
                    kind: BadgeKind::EffectRow,
                },
            ],
        };
        let json = serde_json::to_string(&ui).unwrap();
        assert_eq!(
            json,
            r#"{"ui":"card","title":"hole","body":[{"ui":"code","lang":"haskell","source":"resume :: Verdict -> M ()"},{"ui":"choice","prompt":"verdict?","options":[["approve","Approve"]]},{"ui":"badge","label":"Exec, Fs","kind":"effect_row"}]}"#
        );
    }

    /// A form-field `key`, when present, IS emitted (it's how a multi-field
    /// form's submission gets keyed); when absent it's omitted (backward
    /// compat with standalone widgets). Pins both halves of the contract.
    #[test]
    fn keyed_field_wire_shape() {
        let keyed = Ui::TextIn {
            prompt: "Name".into(),
            multiline: false,
            key: Some("f0".into()),
            initial: None,
        };
        assert_eq!(
            serde_json::to_string(&keyed).unwrap(),
            r#"{"ui":"text_in","prompt":"Name","multiline":false,"key":"f0"}"#
        );
        let unkeyed = Ui::TextIn {
            prompt: "Name".into(),
            multiline: false,
            key: None,
            initial: None,
        };
        assert_eq!(
            serde_json::to_string(&unkeyed).unwrap(),
            r#"{"ui":"text_in","prompt":"Name","multiline":false}"#
        );
    }

    /// `TextIn.initial` (a prefill draft) is emitted only when present, same
    /// skip-serialize discipline as `key` — appended AFTER `key` on the wire.
    #[test]
    fn textin_initial_wire_shape() {
        let prefilled = Ui::TextIn {
            prompt: "Notes".into(),
            multiline: false,
            key: Some("f0".into()),
            initial: Some("draft text".into()),
        };
        assert_eq!(
            serde_json::to_string(&prefilled).unwrap(),
            r#"{"ui":"text_in","prompt":"Notes","multiline":false,"key":"f0","initial":"draft text"}"#
        );
        let no_initial = Ui::TextIn {
            prompt: "Notes".into(),
            multiline: false,
            key: Some("f0".into()),
            initial: None,
        };
        assert_eq!(
            serde_json::to_string(&no_initial).unwrap(),
            r#"{"ui":"text_in","prompt":"Notes","multiline":false,"key":"f0"}"#
        );
    }

    /// `Choice.selected` (a pre-selected default) is emitted only when
    /// present, same skip-serialize discipline as `key` — appended AFTER
    /// `key` on the wire.
    #[test]
    fn choice_selected_wire_shape() {
        let with_default = Ui::Choice {
            prompt: "Lane".into(),
            options: vec![("a".into(), "Alpha".into())],
            key: Some("f0".into()),
            selected: Some("a".into()),
        };
        assert_eq!(
            serde_json::to_string(&with_default).unwrap(),
            r#"{"ui":"choice","prompt":"Lane","options":[["a","Alpha"]],"key":"f0","selected":"a"}"#
        );
        let no_default = Ui::Choice {
            prompt: "Lane".into(),
            options: vec![("a".into(), "Alpha".into())],
            key: Some("f0".into()),
            selected: None,
        };
        assert_eq!(
            serde_json::to_string(&no_default).unwrap(),
            r#"{"ui":"choice","prompt":"Lane","options":[["a","Alpha"]],"key":"f0"}"#
        );
    }

    /// `MultiChoice` wire shape, keyed and unkeyed — mirrors `Choice`'s own
    /// keyed/unkeyed discipline.
    #[test]
    fn multichoice_wire_shape() {
        let keyed = Ui::MultiChoice {
            prompt: "Pick some".into(),
            options: vec![("a".into(), "Alpha".into()), ("b".into(), "Beta".into())],
            key: Some("f0".into()),
        };
        assert_eq!(
            serde_json::to_string(&keyed).unwrap(),
            r#"{"ui":"multi_choice","prompt":"Pick some","options":[["a","Alpha"],["b","Beta"]],"key":"f0"}"#
        );
        let unkeyed = Ui::MultiChoice {
            prompt: "Pick some".into(),
            options: vec![("a".into(), "Alpha".into())],
            key: None,
        };
        assert_eq!(
            serde_json::to_string(&unkeyed).unwrap(),
            r#"{"ui":"multi_choice","prompt":"Pick some","options":[["a","Alpha"]]}"#
        );
    }
}
