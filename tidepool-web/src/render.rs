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
//!
//! ## Recursive rendering — [`generic_shape`]
//!
//! [`panel`]/[`View`]/[`form`] above render the FLAT `FormSpec` — unchanged,
//! and still what a spec with no `shape` takes. [`generic_shape`]
//! renders the RECURSIVE `FormShape`
//! (`tidepool_harness::selfharness::operator`) instead, and [`form`] routes
//! to it whenever the pending spec carries one — which is what `askUser @T`
//! (`plans/self-iterating-harness/14-generic-derived-askuser-prd.md`) emits.
//! It reuses the SAME flat
//! `[data-bind]`/`[data-kind]` collector `shell::JS` already ships — no
//! client JS changes — by binding every leaf at a DOTTED path
//! (`tidepool_harness::selfharness::operator::child_path`) instead of a bare
//! key, e.g. a nested `destination.host` input. `shell::JS`'s collector
//! treats a dotted path as an ordinary (if unusual) object key string; it
//! doesn't need to understand nesting, because `server::collect_form_answer`
//! reassembles the resulting flat `{"destination.host": …}` map back into a
//! structural [`tidepool_harness::selfharness::operator::FormAnswer`] on the
//! server side, guided by the same `FormShape` the form was rendered from.
//!
//! A payload-bearing sum renders the discriminating choice AND every
//! variant's nested payload form, all at once (server-rendered, no
//! client-side branching) — each variant's fields are bound under
//! `<path>.<Constructor>.<field>`, so different branches' fields never
//! collide even though they're all present in the DOM simultaneously.
//! `server::collect_form_answer` reads the chosen constructor and only looks
//! at that branch's fields; a self-contained `<style>` block (CSS `:has()`)
//! visually hides every non-chosen branch so the operator only sees the one
//! they picked, without needing `shell.rs`'s JS to know anything about it.

use maud::{html, Markup, PreEscaped};
use tidepool_harness::selfharness::operator::{
    child_path, humanize_key, Field, FieldKind, FieldShape, FormShape, FormSpec, VariantShape,
    ROOT_BIND_PATH,
};

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
///
/// A spec carrying a recursive `shape` (what `askUser @T` emits) renders
/// through [`generic_shape`] at [`ROOT_BIND_PATH`] instead of the flat field
/// rows — the same `[data-bind]` collector either way, since `generic_shape`
/// binds its leaves at dotted paths. `crate::server::collect_form_answer`
/// reassembles those, guided by this same shape.
fn form(spec: &FormSpec) -> Markup {
    html! {
        form class="form" data-on-submit="@post('/submit')" {
            @if let Some(shape) = &spec.shape {
                (generic_shape(ROOT_BIND_PATH, shape))
            } @else {
                @for (i, field) in spec.fields.iter().enumerate() {
                    (field_row(i + 1, field))
                }
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

// -------------------------------------------------------------------------
// Recursive rendering — FormShape -> Markup (see module docs).
// -------------------------------------------------------------------------

/// Render a recursive [`FormShape`] as a standalone markup fragment, binding
/// every leaf at a dotted path rooted at `path` (see module docs — this
/// reuses `shell::JS`'s flat `[data-bind]` collector unchanged).
#[must_use]
pub fn generic_shape(path: &str, shape: &FormShape) -> Markup {
    match shape {
        FormShape::String => html! {
            input type="text" class="input" data-bind=(path) data-kind="string";
        },
        FormShape::Int => html! {
            input type="number" class="input" data-bind=(path) data-kind="int";
        },
        FormShape::Number => html! {
            input type="number" step="any" class="input" data-bind=(path) data-kind="number";
        },
        FormShape::Bool => html! {
            label class="bool" {
                input type="checkbox" data-bind=(path) data-kind="bool";
                span { "Yes" }
            }
        },
        FormShape::Unit => html! {},
        FormShape::Optional(inner) => generic_optional(path, inner),
        FormShape::Product { fields, .. } => generic_product(path, fields),
        FormShape::Sum { variants, .. } => generic_sum(path, variants),
    }
}

/// An optional group: an "Include" toggle bound at `<path>.__present`
/// (a reserved sentinel key — never a real selector/constructor key, so it
/// never collides, and `server::collect_form_answer` strips it before
/// building the [`tidepool_harness::selfharness::operator::FormAnswer`])
/// plus the inner shape rendered at `path` itself.
fn generic_optional(path: &str, inner: &FormShape) -> Markup {
    let present_key = format!("{path}.__present");
    html! {
        div class="optional" data-node="optional" data-path=(path) {
            label class="optional-toggle" {
                input type="checkbox" data-bind=(present_key) data-kind="bool";
                span { "Include" }
            }
            div class="optional-inner" {
                (generic_shape(path, inner))
            }
        }
    }
}

/// A product: one numbered field row per [`FieldShape`], in declaration
/// order — the DISPLAY label is [`humanize_key`]'d, the bind path underneath
/// carries the exact [`FieldKey`] verbatim.
fn generic_product(path: &str, fields: &[FieldShape]) -> Markup {
    html! {
        div class="product" data-node="product" data-path=(path) {
            @for (i, field) in fields.iter().enumerate() {
                (generic_field_row(i + 1, path, field))
            }
        }
    }
}

fn generic_field_row(index: usize, parent_path: &str, field: &FieldShape) -> Markup {
    let bind_path = child_path(parent_path, &field.key);
    html! {
        div class="field" {
            div class="field-meta" {
                span class="field-index" { (format!("{index:02}")) }
                label class="eyebrow" for=(bind_path) { (humanize_key(&field.key)) }
            }
            div class="field-input" { (generic_shape(&bind_path, &field.shape)) }
        }
    }
}

/// A sum: a discriminating choice over every variant's [`ConstructorKey`]
/// (DISPLAY humanized, submitted value exact), plus — only when at least one
/// variant carries a payload — every payload-bearing variant's nested form,
/// each bound under `<path>.<Constructor>.…`. A self-contained `<style>`
/// block hides every branch except the chosen one via CSS `:has()`, scoped
/// to this sum's own `path` + each variant's exact constructor value so
/// sibling sum fields on the same page never cross-match.
fn generic_sum(path: &str, variants: &[VariantShape]) -> Markup {
    let all_nullary = variants.iter().all(|v| matches!(v.shape, FormShape::Unit));
    html! {
        div class="sum" data-node="sum" data-path=(path) {
            div class="enum" {
                @for v in variants {
                    label class="enum-opt" {
                        input type="radio" name=(path) value=(v.constructor)
                            data-bind=(path) data-kind="enum";
                        span { (humanize_key(&v.constructor)) }
                    }
                }
            }
            @if !all_nullary {
                style { (PreEscaped(variant_reveal_css(path, variants))) }
                @for v in variants {
                    @if !matches!(v.shape, FormShape::Unit) {
                        div class="variant-payload" data-for=(v.constructor) {
                            (generic_shape(&child_path(path, &v.constructor), &v.shape))
                        }
                    }
                }
            }
        }
    }
}

/// CSS-only reveal: every `.variant-payload` starts hidden; the one whose
/// `data-for` matches the checked radio's value (scoped to THIS sum's
/// `data-bind`, so a same-named constructor in a different sum never
/// matches) is shown. No `shell.rs` JS changes needed.
fn variant_reveal_css(path: &str, variants: &[VariantShape]) -> String {
    let mut css = String::from(".variant-payload { display: none; }\n");
    for v in variants {
        if matches!(v.shape, FormShape::Unit) {
            continue;
        }
        css.push_str(&format!(
            ".sum[data-path=\"{path}\"]:has(input[data-bind=\"{path}\"][value=\"{ctor}\"]:checked) \
             > .variant-payload[data-for=\"{ctor}\"] {{ display: block; }}\n",
            path = css_escape(path),
            ctor = css_escape(&v.constructor),
        ));
    }
    css
}

/// Escape `"` and `\` for embedding inside a double-quoted CSS attribute
/// selector string.
fn css_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
            shape: None,
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

    // ---- generic_shape ------------------------------------------------------

    use tidepool_harness::selfharness::operator::{FieldShape, FormShape, VariantShape};

    #[test]
    fn generic_shape_renders_leaves_with_bind_and_kind() {
        let html = generic_shape("count", &FormShape::Int).into_string();
        assert!(html.contains("data-bind=\"count\""));
        assert!(html.contains("data-kind=\"int\""));

        let html = generic_shape("ok", &FormShape::Bool).into_string();
        assert!(html.contains("data-bind=\"ok\""));
        assert!(html.contains("data-kind=\"bool\""));
    }

    #[test]
    fn generic_shape_renders_all_nullary_sum_as_compact_choice() {
        let shape = FormShape::Sum {
            type_key: "Environment".to_string(),
            variants: vec![
                VariantShape {
                    constructor: "Development".to_string(),
                    shape: FormShape::Unit,
                },
                VariantShape {
                    constructor: "Staging".to_string(),
                    shape: FormShape::Unit,
                },
            ],
        };
        let html = generic_shape("environment", &shape).into_string();
        assert!(html.contains("value=\"Development\""));
        assert!(html.contains("value=\"Staging\""));
        assert!(html.contains("data-bind=\"environment\""));
        // all-nullary: no payload markup, no reveal <style>
        assert!(!html.contains("variant-payload"));
    }

    #[test]
    fn generic_shape_renders_optional_group() {
        let shape = FormShape::Optional(Box::new(FormShape::String));
        let html = generic_shape("releaseNote", &shape).into_string();
        assert!(html.contains("data-bind=\"releaseNote.__present\""));
        assert!(html.contains("data-kind=\"bool\""));
        assert!(html.contains("data-bind=\"releaseNote\""));
        assert!(html.contains("data-kind=\"string\""));
        assert!(html.contains("Include"));
    }

    fn destination_sum_shape() -> FormShape {
        FormShape::Sum {
            type_key: "Destination".to_string(),
            variants: vec![
                VariantShape {
                    constructor: "LocalHost".to_string(),
                    shape: FormShape::Unit,
                },
                VariantShape {
                    constructor: "Ssh".to_string(),
                    shape: FormShape::Product {
                        type_key: "Ssh".to_string(),
                        constructor: "Ssh".to_string(),
                        fields: vec![
                            FieldShape {
                                key: "host".to_string(),
                                shape: FormShape::String,
                            },
                            FieldShape {
                                key: "port".to_string(),
                                shape: FormShape::Int,
                            },
                        ],
                    },
                },
            ],
        }
    }

    #[test]
    fn generic_shape_renders_payload_bearing_sum_branch_with_nested_form() {
        let html = generic_shape("destination", &destination_sum_shape()).into_string();
        // discriminating choice, exact constructor values
        assert!(html.contains("value=\"LocalHost\""));
        assert!(html.contains("value=\"Ssh\""));
        // the Ssh branch's nested fields, bound under destination.Ssh.<field>
        assert!(html.contains("data-bind=\"destination.Ssh.host\""));
        assert!(html.contains("data-bind=\"destination.Ssh.port\""));
        // LocalHost is nullary — no nested payload markup for it
        assert!(!html.contains("data-for=\"LocalHost\""));
        assert!(html.contains("data-for=\"Ssh\""));
        // a self-contained CSS reveal, scoped to this sum's own path+value
        assert!(html.contains("<style>"));
        assert!(html.contains(":has(input[data-bind=\"destination\"][value=\"Ssh\"]:checked)"));
    }

    /// F4/DONE: DISPLAY labels are humanized (`releaseNote` -> "Release
    /// note", `NeedsReview`-style constructors -> "Needs review") while every
    /// submitted bind path keeps the exact source key. Ties render + collect
    /// together: paths this test scrapes out of the rendered HTML are fed
    /// straight into `collect_form_answer` in `server.rs`'s own test of this
    /// split, so a drift between the two would fail there.
    #[test]
    fn generic_shape_humanizes_labels_but_keeps_exact_bind_keys() {
        let shape = FormShape::Product {
            type_key: "DeployRequest".to_string(),
            constructor: "DeployRequest".to_string(),
            fields: vec![
                FieldShape {
                    key: "releaseNote".to_string(),
                    shape: FormShape::Optional(Box::new(FormShape::String)),
                },
                FieldShape {
                    key: "environment".to_string(),
                    shape: FormShape::Sum {
                        type_key: "Environment".to_string(),
                        variants: vec![VariantShape {
                            constructor: "NeedsReview".to_string(),
                            shape: FormShape::Unit,
                        }],
                    },
                },
            ],
        };
        let html = generic_shape("", &shape).into_string();

        // exact keys survive verbatim in every bind path
        assert!(html.contains("data-bind=\"releaseNote.__present\""));
        assert!(html.contains("data-bind=\"releaseNote\""));
        assert!(html.contains("data-bind=\"environment\""));
        assert!(html.contains("value=\"NeedsReview\""));

        // display text is humanized, never the raw key
        assert!(html.contains("Release note"));
        assert!(html.contains("Needs review"));
        assert!(!html.contains(">releaseNote<"));
        assert!(!html.contains(">NeedsReview<"));
    }
}
