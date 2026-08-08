//! `FormSpec` → maud markup for the operator panel.
//!
//! FROZEN SEAM (consumed by [`crate::server`]): the [`View`] enum and
//! [`panel`] function. `panel` always yields the single `id="panel"` element
//! the SSE stream patches in place — the page shell embeds it once, and every
//! SSE frame replaces it wholesale. The `data-rev` attribute it stamps is
//! part of that wire contract too — [`crate::shell`]'s client JS compares it
//! against the currently-mounted `#panel` to decide whether a focus-preserving
//! skip applies (see `shell.rs`'s `applyPatch`).
//!
//! ## Field wire contract (partner to `shell::JS`'s collector)
//! Every input carries `data-bind="<key>"` and `data-kind="enum|int|text|bool"`.
//! The client JS reads those to collect a FLAT `{ <key>: <scalar> }` submission
//! (enum → chosen `tag` string, int → number, text → string, bool → boolean)
//! and POSTs it to `/submit`.

use maud::{html, Markup};
use tidepool_harness::selfharness::operator::{Field, FieldKind, FormSpec};

/// What the operator panel is currently showing.
pub enum View<'a> {
    /// Nothing pending — the driver is between operator interactions.
    Idle,
    /// A pending `askUser` form: render the fields + a Submit button.
    Form(&'a FormSpec),
    /// The between-loops gate: render a single Continue button.
    Continue,
}

/// The `id="panel"` fragment — the one element patched over SSE. Rendered both
/// into the initial page ([`crate::shell::page`]) and into every SSE frame.
/// `rev` is the server's monotonically-bumped revision counter
/// ([`crate::server::AppState`]), stamped as `data-rev` so the client can
/// tell a genuinely NEW pending interaction (always replace `#panel`) apart
/// from a same-interaction re-render (skip while the operator has focus
/// inside it).
pub fn panel(view: &View, rev: u64) -> Markup {
    html! {
        div id="panel" data-rev=(rev) {
            @match view {
                View::Idle => (idle()),
                View::Form(spec) => (form(spec)),
                View::Continue => (continue_prompt()),
            }
        }
    }
}

/// Idle placeholder — nothing needs the operator right now. A large, quiet
/// glyph and a hairline frame read as a composed sheet, not an empty state.
fn idle() -> Markup {
    html! {
        div class="idle" {
            p class="eyebrow" { "Standby" }
            p class="idle-glyph" { "—" }
            p class="idle-note" { "No operator input pending. The harness is thinking." }
        }
    }
}

/// The pending form: one numbered row per field, then a Submit button. The
/// whole thing is a `data-on-submit="@post('/submit')"` form so the vendored
/// JS collects every `[data-bind]` into a flat object and POSTs it.
fn form(spec: &FormSpec) -> Markup {
    html! {
        form class="form" data-on-submit="@post('/submit')" {
            @for (i, field) in spec.fields.iter().enumerate() {
                (field_row(i + 1, field))
            }
            div class="actions" {
                button type="submit" class="btn btn-primary" { "Submit" }
            }
        }
    }
}

/// One field: an index number + eyebrow label in a fixed left column, the
/// input in the right column — a numbered-list poster grid, not a stacked
/// form.
fn field_row(index: usize, field: &Field) -> Markup {
    html! {
        div class="field" {
            div class="field-meta" {
                span class="field-index" { (format!("{index:02}")) }
                label class="eyebrow" for=(field.key) { (field.label) }
            }
            div class="field-input" { (field_input(field)) }
        }
    }
}

fn field_input(field: &Field) -> Markup {
    let key = field.key.as_str();
    match &field.kind {
        FieldKind::Enum { options } => html! {
            div class="enum" {
                @for opt in options {
                    label class="enum-opt" {
                        input type="radio" name=(key) value=(opt.tag)
                            data-bind=(key) data-kind="enum";
                        span { (opt.label) }
                    }
                }
            }
        },
        FieldKind::Int => html! {
            input id=(key) type="number" class="input"
                data-bind=(key) data-kind="int";
        },
        FieldKind::Text => html! {
            input id=(key) type="text" class="input"
                data-bind=(key) data-kind="text";
        },
        FieldKind::Bool => html! {
            label class="bool" {
                input type="checkbox" data-bind=(key) data-kind="bool";
                span { "Yes" }
            }
        },
    }
}

/// The between-loops continue gate: a single button POSTing `/continue`.
fn continue_prompt() -> Markup {
    html! {
        div class="continue" {
            p class="eyebrow" { "Loop complete — awaiting operator" }
            button class="btn btn-primary" data-on-click="@post('/continue')" {
                "Continue"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_harness::selfharness::operator::{EnumOption, Field, FieldKind};

    fn sample() -> FormSpec {
        FormSpec {
            fields: vec![
                Field {
                    key: "mood".into(),
                    label: "Mood".into(),
                    kind: FieldKind::Enum {
                        options: vec![
                            EnumOption {
                                label: "Calm".into(),
                                tag: "calm".into(),
                            },
                            EnumOption {
                                label: "Busy".into(),
                                tag: "busy".into(),
                            },
                        ],
                    },
                },
                Field {
                    key: "count".into(),
                    label: "Count".into(),
                    kind: FieldKind::Int,
                },
                Field {
                    key: "note".into(),
                    label: "Note".into(),
                    kind: FieldKind::Text,
                },
                Field {
                    key: "ok".into(),
                    label: "OK?".into(),
                    kind: FieldKind::Bool,
                },
            ],
        }
    }

    #[test]
    fn panel_form_renders_every_field_kind_with_bind_and_kind() {
        let spec = sample();
        let html = panel(&View::Form(&spec), 0).into_string();
        assert!(html.starts_with("<div id=\"panel\""));
        // one data-bind per field key
        for key in ["mood", "count", "note", "ok"] {
            assert!(
                html.contains(&format!("data-bind=\"{key}\"")),
                "missing bind {key}"
            );
        }
        // enum options submit their tags, show their labels
        assert!(html.contains("value=\"calm\""));
        assert!(html.contains("Calm"));
        // kinds present for the JS coercer
        for kind in ["enum", "int", "text", "bool"] {
            assert!(
                html.contains(&format!("data-kind=\"{kind}\"")),
                "missing kind {kind}"
            );
        }
        assert!(html.contains("@post('/submit')"));
    }

    #[test]
    fn panel_continue_renders_button() {
        let html = panel(&View::Continue, 0).into_string();
        assert!(html.contains("@post('/continue')"));
    }

    #[test]
    fn panel_idle_is_quiet() {
        let html = panel(&View::Idle, 0).into_string();
        assert!(html.starts_with("<div id=\"panel\""));
        assert!(!html.contains("@post"));
    }

    /// F10: `panel()` stamps the caller's revision as `data-rev` on the
    /// `#panel` root, and a different revision produces a different
    /// attribute value — the signal `shell.rs`'s client JS keys its
    /// skip-on-focus rule off.
    #[test]
    fn panel_stamps_data_rev_from_the_argument() {
        let a = panel(&View::Idle, 7).into_string();
        assert!(a.contains("id=\"panel\" data-rev=\"7\""), "{a}");

        let b = panel(&View::Idle, 8).into_string();
        assert!(b.contains("id=\"panel\" data-rev=\"8\""), "{b}");
        assert_ne!(a, b);
    }
}
