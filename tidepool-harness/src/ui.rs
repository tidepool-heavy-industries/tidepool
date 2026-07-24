//! The `Ui` eDSL wire mirror — the contract between the Haskell side
//! (`haskell/lib/Tidepool/Ui.hs`, whose hand-written ToJSON must emit
//! EXACTLY this serde shape) and the tidepool-web renderer. Haskell
//! speaks elicitation semantics; Datastar/HTML vocabulary exists only in
//! the renderer. R0 is first-order (no monadic sequencing — that grows in
//! R1 with the Dialog effect).
//!
//! LAW (B2, open sum): every `Choice` elicitation renders with an
//! open-prose escape path IN ADDITION to its options — the renderer adds
//! it unconditionally; it is not represented (and not omittable) here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ui", rename_all = "snake_case")]
pub enum Ui {
    Card { title: String, body: Vec<Ui> },
    /// Markdown.
    Prose { text: String },
    /// Fenced source block — type signatures, decls, eval sources.
    Code { lang: String, source: String },
    Choice {
        prompt: String,
        /// (key, label) pairs; the answer references the key.
        options: Vec<(String, String)>,
    },
    TextIn { prompt: String, multiline: bool },
    Badge { label: String, kind: BadgeKind },
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
                Ui::Code { lang: "haskell".into(), source: "resume :: Verdict -> M ()".into() },
                Ui::Choice {
                    prompt: "verdict?".into(),
                    options: vec![("approve".into(), "Approve".into())],
                },
                Ui::Badge { label: "Exec, Fs".into(), kind: BadgeKind::EffectRow },
            ],
        };
        let json = serde_json::to_string(&ui).unwrap();
        assert_eq!(
            json,
            r#"{"ui":"card","title":"hole","body":[{"ui":"code","lang":"haskell","source":"resume :: Verdict -> M ()"},{"ui":"choice","prompt":"verdict?","options":[["approve","Approve"]]},{"ui":"badge","label":"Exec, Fs","kind":"effect_row"}]}"#
        );
    }
}
