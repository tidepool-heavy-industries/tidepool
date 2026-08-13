//! `FormShape` → maud markup for the operator panel.
//!
//! [`View`] and [`panel`] are consumed by [`crate::server`]. `panel` always
//! yields the single `id="panel"` element
//! the SSE stream patches in place — the page shell embeds it once, and every
//! SSE frame replaces it wholesale. The `data-rev` attribute it stamps is
//! part of that wire contract too — [`crate::shell`]'s client JS compares it
//! against the currently-mounted `#panel` to decide whether a focus-preserving
//! skip applies (see `shell.rs`'s `applyPatch`).
//!
//! Every input carries a dotted `data-bind` path and a scalar `data-kind`.
//! The client collects those controls into a flat object and POSTs it to
//! `/submit`; [`crate::server::collect_form_json`] validates and reassembles
//! the object using the same [`FormShape`] rendered here. Leaves bind at
//! paths built by `tidepool_harness::selfharness::operator::child_path`,
//! e.g. a nested `destination.host` input. `shell::JS`'s collector
//! treats a dotted path as an ordinary (if unusual) object key string; it
//! doesn't need to understand nesting, because `server::collect_form_json`
//! reassembles the resulting flat `{"destination.host": …}` map back into a
//! plain JSON answer (`server.rs::collect_form_json`) on the
//! server side, guided by the same `FormShape` the form was rendered from.
//!
//! A payload-bearing sum renders the discriminating choice and every
//! variant's nested payload form, all at once (server-rendered, no
//! client-side branching) — each variant's fields are bound under
//! `<path>.<Constructor>.<field>`, so different branches' fields never
//! collide even though they're all present in the DOM simultaneously.
//! `server::collect_form_json` reads the chosen constructor and only looks
//! at that branch's fields; a self-contained `<style>` block (CSS `:has()`)
//! visually hides every non-chosen branch so the operator only sees the one
//! they picked, without needing `shell.rs`'s JS to know anything about it.

use maud::{html, Markup, PreEscaped};
use tidepool_harness::selfharness::operator::{
    child_path, humanize_key, FieldShape, FormShape, VariantShape, ROOT_BIND_PATH,
};

/// What the operator panel is currently showing.
pub enum View<'a> {
    /// Nothing pending — the driver is between operator interactions.
    Idle,
    /// A pending `askUser` form: render the fields + a Submit button.
    Form(&'a FormShape),
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
///
/// `notes` is the current loop's accumulated `note` feed (empty renders
/// nothing), shown ABOVE the form/idle/continue view — narration explaining
/// what is about to be asked and why belongs before the thing it explains.
/// `last_turn_source` is the most recently compiled answerer round's
/// Haskell, shown BELOW as a collapsed `<details>` pane so it never crowds
/// the form.
pub fn panel(view: &View, notes: &[String], last_turn_source: Option<&str>, rev: u64) -> Markup {
    html! {
        div id="panel" data-rev=(rev) {
            @if !notes.is_empty() {
                (notes_feed(notes))
            }
            @match view {
                View::Idle => (idle()),
                View::Form(shape) => (form(shape)),
                View::Continue => (continue_prompt()),
            }
            @if let Some(source) = last_turn_source {
                (turn_source_pane(source))
            }
        }
    }
}

/// The accumulated `note` feed: one paragraph per posted note, in post
/// order, plain text (no markdown — `white-space: pre-wrap` alone carries
/// blank-line paragraph breaks the author wrote). maud escapes `note` as an
/// ordinary text node.
fn notes_feed(notes: &[String]) -> Markup {
    html! {
        div class="notes" data-node="notes" {
            @for note in notes {
                p class="note" style="white-space: pre-wrap" { (note) }
            }
        }
    }
}

/// The last-turn-source pane: collapsed by default so it never crowds the
/// form/notes above it. Plain `<pre>` v1, no syntax highlighting; `source` is
/// interpolated as an ordinary maud text node (escaped), never `PreEscaped`.
fn turn_source_pane(source: &str) -> Markup {
    html! {
        details class="turn-source" data-node="turn-source" {
            summary { "Last turn's Haskell" }
            pre { (source) }
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

/// The pending form. The browser collector posts its leaf controls as a flat
/// dotted-path object; the server reassembles it using this same shape.
fn form(shape: &FormShape) -> Markup {
    html! {
        form class="form" data-on-submit="@post('/submit')" {
            (generic_shape(ROOT_BIND_PATH, shape))
            div class="actions" {
                button type="submit" class="btn btn-primary" { "Submit" }
            }
        }
    }
}

/// The between-loops continue gate: an OPTIONAL message plus the advance —
/// the operator's one channel for initiating (`ContinueSignal`). An empty
/// box submits as a bare continue; text rides as `{"input": ...}` and
/// reaches the next cognition window's framing. Rendered as a
/// `data-on-submit` form so the shared collector gathers the field.
fn continue_prompt() -> Markup {
    html! {
        form class="continue" data-on-submit="@post('/continue')" {
            p class="eyebrow" { "Loop complete — awaiting operator" }
            textarea
                class="continue-input"
                data-bind="input"
                data-kind="string"
                rows="3"
                placeholder="Say something to the companion (optional) — it arrives with the next window"
            {}
            button type="submit" class="btn btn-primary" { "Continue" }
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
        // Placeholder because a ROOT primitive renders with no field label
        // above it (nothing in the shape carries question text yet) — without
        // it the form reads as a blank page until the input is focused.
        FormShape::String => html! {
            input type="text" class="input" data-bind=(path) data-kind="string"
                placeholder="Type your answer…";
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

/// An optional group: an "Include" toggle bound at `<path>#present`
/// (`#` cannot occur in a Haskell record selector, so this UI-only key never
/// collides, and `server::collect_form_json` strips it before
/// building the plain JSON answer)
/// plus the inner shape rendered at `path` itself.
fn generic_optional(path: &str, inner: &FormShape) -> Markup {
    let present_key = format!("{path}#present");
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
    let all_nullary = variants.iter().all(|v| is_nullary_variant(&v.shape));
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
                    @if !is_nullary_variant(&v.shape) {
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
/// `data-for` matches the checked radio's value (scoped to this sum's
/// `data-bind`, so a same-named constructor in a different sum never
/// matches) is shown. No `shell.rs` JS changes needed.
fn variant_reveal_css(path: &str, variants: &[VariantShape]) -> String {
    let mut css = String::from(".variant-payload { display: none; }\n");
    for v in variants {
        if is_nullary_variant(&v.shape) {
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

fn is_nullary_variant(shape: &FormShape) -> bool {
    matches!(shape, FormShape::Product { fields, .. } if fields.is_empty())
}

/// Escape `"` and `\` for embedding inside a double-quoted CSS attribute
/// selector string.
fn css_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_continue_renders_button() {
        let html = panel(&View::Continue, &[], None, 0).into_string();
        assert!(html.contains("@post('/continue')"));
    }

    #[test]
    fn panel_idle_is_quiet() {
        let html = panel(&View::Idle, &[], None, 0).into_string();
        assert!(html.starts_with("<div id=\"panel\""));
        assert!(!html.contains("@post"));
    }

    /// F10: `panel()` stamps the caller's revision as `data-rev` on the
    /// `#panel` root, and a different revision produces a different
    /// attribute value — the signal `shell.rs`'s client JS keys its
    /// skip-on-focus rule off.
    #[test]
    fn panel_stamps_data_rev_from_the_argument() {
        let a = panel(&View::Idle, &[], None, 7).into_string();
        assert!(a.contains("id=\"panel\" data-rev=\"7\""), "{a}");

        let b = panel(&View::Idle, &[], None, 8).into_string();
        assert!(b.contains("id=\"panel\" data-rev=\"8\""), "{b}");
        assert_ne!(a, b);
    }

    /// Notes render ABOVE the form, in post order, escaped as ordinary text.
    #[test]
    fn panel_renders_notes_above_the_form() {
        let notes = vec!["first note".to_string(), "<b>second</b> note".to_string()];
        let html = panel(&View::Idle, &notes, None, 0).into_string();
        let notes_pos = html.find("first note").expect("first note rendered");
        let second_pos = html.find("second").expect("second note rendered");
        let idle_pos = html.find("Standby").expect("idle view still rendered");
        assert!(
            notes_pos < idle_pos && second_pos < idle_pos,
            "notes must render above the form/idle view:\n{html}"
        );
        assert!(
            html.contains("&lt;b&gt;second&lt;/b&gt; note"),
            "a note must be escaped as an ordinary text node: {html}"
        );
    }

    /// An empty note feed adds no notes markup at all.
    #[test]
    fn panel_with_no_notes_renders_no_notes_node() {
        let html = panel(&View::Idle, &[], None, 0).into_string();
        assert!(!html.contains("data-node=\"notes\""), "{html}");
    }

    /// The last-turn-source pane is a COLLAPSED `<details>` below the
    /// form/idle view, with its source escaped as an ordinary text node
    /// (never `PreEscaped`) — a source containing `<script>` must not survive
    /// as live markup.
    #[test]
    fn panel_renders_last_turn_source_collapsed_and_escaped() {
        let source = "resume (Approve :: Decision) -- <script>alert(1)</script>";
        let html = panel(&View::Idle, &[], Some(source), 0).into_string();
        assert!(html.contains("<details"), "{html}");
        assert!(html.contains("Last turn's Haskell"), "{html}");
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        let idle_pos = html.find("Standby").expect("idle view still rendered");
        let details_pos = html.find("<details").expect("details rendered");
        assert!(
            idle_pos < details_pos,
            "the turn-source pane must render BELOW the form/idle view:\n{html}"
        );
    }

    /// `None` adds no turn-source markup at all.
    #[test]
    fn panel_with_no_turn_source_renders_no_details() {
        let html = panel(&View::Idle, &[], None, 0).into_string();
        assert!(!html.contains("<details"), "{html}");
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
                    shape: empty_product("Environment", "Development"),
                },
                VariantShape {
                    constructor: "Staging".to_string(),
                    shape: empty_product("Environment", "Staging"),
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
        assert!(html.contains("data-bind=\"releaseNote#present\""));
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
                    shape: empty_product("Destination", "LocalHost"),
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
    /// straight into `collect_form_json` in `server.rs`'s own test of this
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
                            shape: empty_product("Environment", "NeedsReview"),
                        }],
                    },
                },
            ],
        };
        let html = generic_shape("", &shape).into_string();

        // exact keys survive verbatim in every bind path
        assert!(html.contains("data-bind=\"releaseNote#present\""));
        assert!(html.contains("data-bind=\"releaseNote\""));
        assert!(html.contains("data-bind=\"environment\""));
        assert!(html.contains("value=\"NeedsReview\""));

        // display text is humanized, never the raw key
        assert!(html.contains("Release note"));
        assert!(html.contains("Needs review"));
        assert!(!html.contains(">releaseNote<"));
        assert!(!html.contains(">NeedsReview<"));
    }

    fn empty_product(type_key: &str, constructor: &str) -> FormShape {
        FormShape::Product {
            type_key: type_key.to_string(),
            constructor: constructor.to_string(),
            fields: vec![],
        }
    }
}
