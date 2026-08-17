//! `FormShape` → maud markup for a NODE's operator panel — stacking every
//! ask currently pending for that node.
//!
//! [`Ask`] and [`node_panel`] are consumed by [`crate::server`]. `node_panel`
//! always yields a single `id="panel-<node_id>"` element — the unit the SSE
//! stream patches in place, and the page shell embeds one per registered
//! node. The `data-rev` attribute it stamps on that root is part of the wire
//! contract too — [`crate::shell`]'s client JS compares it against the
//! currently-mounted `#panel-<node_id>`'s to decide whether a
//! focus-preserving skip applies (see `shell.rs`'s `applyPatch`). Each
//! STACKED ask within also carries its OWN `data-rev` — stable for the ask's
//! whole pending lifetime (its interaction id never changes), so the
//! identity of an untouched ask survives a sibling ask's arrival/resolution
//! or a note/turn-history update even though the whole panel replaces
//! wholesale on any of those.
//!
//! Every input carries a dotted `data-bind` path and a scalar `data-kind`.
//! The client collects those controls into a flat object and POSTs it to
//! `/node/<node>/submit/<interaction>` (baked directly into the rendered
//! `@post('...')` literal — no client-side URL assembly);
//! [`crate::server::collect_form_json`] validates and reassembles the object
//! using the same [`FormShape`] rendered here. Leaves bind at paths built by
//! `tidepool_harness::selfharness::operator::child_path`, e.g. a nested
//! `destination.host` input. `shell::JS`'s collector treats a dotted path as
//! an ordinary (if unusual) object key string; it doesn't need to understand
//! nesting, because `server::collect_form_json` reassembles the resulting
//! flat `{"destination.host": …}` map back into a plain JSON answer on the
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

/// One ask currently pending for a node — either an `askUser` form or the
/// between-loops continue gate.
pub enum Ask<'a> {
    Form(&'a FormShape),
    Continue,
}

/// The `id="panel-<node_id>"` fragment for one node — the one element the
/// SSE stream patches in place for that node. Rendered both into the initial
/// page ([`crate::shell::page`]) and into every SSE frame for `node_id`.
///
/// `asks` is every currently pending interaction for `node_id`, in publish
/// order — rendered STACKED (never just the newest; an operator gate never
/// hides a question). An empty `asks` renders the idle placeholder. `rev` is
/// the node's aggregate revision ([`crate::server::AppState`]), stamped as
/// `data-rev` on the panel root so the client can tell a genuinely NEW
/// interaction (always replace `#panel-<node_id>`) apart from a
/// same-interaction-set re-render (skip while the operator has focus inside
/// it).
///
/// `notes` is the current loop's accumulated `note` feed (empty renders
/// nothing), shown ABOVE the asks — narration explaining what is about to be
/// asked and why belongs before the thing it explains. `turn_history` is
/// every compiled answerer round's Haskell in post order (oldest first,
/// empty renders nothing), shown BELOW the asks as the scrollable
/// turn-history pane.
pub fn node_panel(
    node_id: &str,
    asks: &[(u64, Ask)],
    notes: &[String],
    turn_history: &[String],
    rev: u64,
) -> Markup {
    html! {
        div id=(panel_id(node_id)) data-rev=(rev) {
            @if !notes.is_empty() {
                (notes_feed(notes))
            }
            @if asks.is_empty() {
                (idle())
            } @else {
                div class="asks" data-node="asks" {
                    @for (interaction, ask) in asks {
                        (ask_view(node_id, *interaction, ask))
                    }
                }
            }
            @if !turn_history.is_empty() {
                (turn_history_pane(turn_history))
            }
        }
    }
}

/// The DOM id a node's panel root carries — `id="panel-<node_id>"`.
#[must_use]
pub fn panel_id(node_id: &str) -> String {
    format!("panel-{node_id}")
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

/// The turn-history pane, below the asks/notes: one nested `<details>` per
/// compiled turn, NEWEST FIRST with only the newest open — the operator
/// scrolls back through past turns inside a bounded-height container
/// (`.turn-history` CSS), so an open pane never crowds the asks. Entries are
/// numbered in post order (turn 1 = oldest still retained; the server caps
/// the history, so numbering restarts only across process restarts). Plain
/// `<pre>`, no syntax highlighting; every source string is interpolated as
/// an ordinary maud text node (escaped), never `PreEscaped`.
fn turn_history_pane(history: &[String]) -> Markup {
    let n = history.len();
    html! {
        details class="turn-source" data-node="turn-source" open {
            summary { "Haskell turns (" (n) ")" }
            div class="turn-history" {
                @for (i, source) in history.iter().enumerate().rev() {
                    details class="turn-entry" data-node="turn-entry" open[i + 1 == n] {
                        summary { "turn " (i + 1) " — " (snippet(source)) }
                        pre { (source) }
                    }
                }
            }
        }
    }
}

/// A one-line teaser for a turn entry's summary: the source's first line,
/// truncated. Rendered as a text node like everything else.
fn snippet(source: &str) -> String {
    let head = source.lines().next().unwrap_or("").trim();
    let mut s: String = head.chars().take(64).collect();
    if head.chars().count() > 64 {
        s.push('…');
    }
    s
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

/// One stacked ask: a form or the continue gate, dispatched by kind.
fn ask_view(node_id: &str, interaction: u64, ask: &Ask) -> Markup {
    match ask {
        Ask::Form(shape) => ask_form(node_id, interaction, shape),
        Ask::Continue => ask_continue(node_id, interaction),
    }
}

/// One pending form, addressed by `node_id`/`interaction` in its own
/// `@post(...)` target — the client posts its collected flat controls
/// straight to the exact ask that produced them, no separate nonce field
/// needed in the body.
fn ask_form(node_id: &str, interaction: u64, shape: &FormShape) -> Markup {
    html! {
        form id=(ask_id(node_id, interaction)) data-rev=(interaction) class="form"
             data-on-submit=(post_url(node_id, "submit", interaction)) {
            (generic_shape(ROOT_BIND_PATH, shape))
            div class="actions" {
                button type="submit" class="btn btn-primary" { "Submit" }
            }
        }
    }
}

/// The [`ContinueSignal`] sum's own [`FormShape`] — the between-loops gate
/// rendered and reassembled through the SAME generic sum machinery as every
/// `askUser` form (one presentation algebra, no bespoke pane): two variants
/// with different subfields, radio-picked, the payload branch revealing its
/// text field. Shared with `server::continue_loop`'s reassembly so render
/// and decode cannot drift. Adding a variant here (and an arm to the
/// server's mapping) is the WHOLE cost of a new between-loops action.
pub fn continue_shape() -> FormShape {
    FormShape::Sum {
        type_key: "ContinueSignal".to_string(),
        variants: vec![
            VariantShape {
                constructor: "Continue".to_string(),
                shape: FormShape::Product {
                    type_key: "ContinueSignal".to_string(),
                    constructor: "Continue".to_string(),
                    fields: vec![],
                },
            },
            VariantShape {
                constructor: "ContinueWithInput".to_string(),
                shape: FormShape::Product {
                    type_key: "ContinueSignal".to_string(),
                    constructor: "ContinueWithInput".to_string(),
                    fields: vec![FieldShape {
                        key: "input".to_string(),
                        shape: FormShape::String,
                    }],
                },
            },
        ],
    }
}

/// One pending between-loops continue gate: the [`continue_shape`] sum
/// rendered by the generic machinery, POSTing to
/// `/node/<node_id>/continue/<interaction>`.
fn ask_continue(node_id: &str, interaction: u64) -> Markup {
    html! {
        form id=(ask_id(node_id, interaction)) data-rev=(interaction) class="continue"
             data-on-submit=(post_url(node_id, "continue", interaction)) {
            p class="eyebrow" { "Loop complete — awaiting operator" }
            (generic_shape(ROOT_BIND_PATH, &continue_shape()))
            button type="submit" class="btn btn-primary" { "Continue" }
        }
    }
}

/// The DOM id one stacked ask carries — `id="ask-<node_id>-<interaction>"`.
fn ask_id(node_id: &str, interaction: u64) -> String {
    format!("ask-{node_id}-{interaction}")
}

/// The literal `@post('...')` target `shell::JS`'s vendored client parses out
/// of a `data-on-submit` attribute — node- and interaction-scoped, baked in
/// at render time so no client-side URL assembly is needed.
fn post_url(node_id: &str, verb: &str, interaction: u64) -> String {
    format!("@post('/node/{node_id}/{verb}/{interaction}')")
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
    fn node_panel_continue_renders_button() {
        let asks = vec![(0u64, Ask::Continue)];
        let html = node_panel("n1", &asks, &[], &[], 0).into_string();
        assert!(html.contains("@post('/node/n1/continue/0')"));
    }

    #[test]
    fn node_panel_idle_is_quiet() {
        let html = node_panel("n1", &[], &[], &[], 0).into_string();
        assert!(html.starts_with("<div id=\"panel-n1\""));
        assert!(!html.contains("@post"));
    }

    /// F10 (per-node): `node_panel()` stamps the caller's revision as
    /// `data-rev` on the panel root, and a different revision produces a
    /// different attribute value.
    #[test]
    fn node_panel_stamps_data_rev_from_the_argument() {
        let a = node_panel("n1", &[], &[], &[], 7).into_string();
        assert!(a.contains("id=\"panel-n1\" data-rev=\"7\""), "{a}");

        let b = node_panel("n1", &[], &[], &[], 8).into_string();
        assert!(b.contains("id=\"panel-n1\" data-rev=\"8\""), "{b}");
        assert_ne!(a, b);
    }

    /// The panel id is node-scoped, distinguishing tabs.
    #[test]
    fn node_panel_id_is_scoped_per_node() {
        let a = node_panel("alpha", &[], &[], &[], 0).into_string();
        let b = node_panel("beta", &[], &[], &[], 0).into_string();
        assert!(a.starts_with("<div id=\"panel-alpha\""));
        assert!(b.starts_with("<div id=\"panel-beta\""));
    }

    /// The core stacking behavior: two pending asks on one node BOTH render,
    /// each with its own stable id/data-rev keyed on its interaction id —
    /// an operator gate never hides a question by only showing the newest.
    #[test]
    fn node_panel_stacks_every_pending_ask_with_its_own_rev() {
        let shape = FormShape::String;
        let asks = vec![(3u64, Ask::Form(&shape)), (7u64, Ask::Continue)];
        let html = node_panel("n1", &asks, &[], &[], 42).into_string();

        assert!(html.contains("id=\"ask-n1-3\" data-rev=\"3\""), "{html}");
        assert!(html.contains("id=\"ask-n1-7\" data-rev=\"7\""), "{html}");
        assert!(html.contains("@post('/node/n1/submit/3')"), "{html}");
        assert!(html.contains("@post('/node/n1/continue/7')"), "{html}");
        // The panel root's own aggregate revision is distinct from either
        // ask's individual one.
        assert!(html.contains("id=\"panel-n1\" data-rev=\"42\""), "{html}");
    }

    /// Notes render ABOVE the asks, in post order, escaped as ordinary text.
    #[test]
    fn node_panel_renders_notes_above_the_asks() {
        let notes = vec!["first note".to_string(), "<b>second</b> note".to_string()];
        let html = node_panel("n1", &[], &notes, &[], 0).into_string();
        let notes_pos = html.find("first note").expect("first note rendered");
        let second_pos = html.find("second").expect("second note rendered");
        let idle_pos = html.find("Standby").expect("idle view still rendered");
        assert!(
            notes_pos < idle_pos && second_pos < idle_pos,
            "notes must render above the asks/idle view:\n{html}"
        );
        assert!(
            html.contains("&lt;b&gt;second&lt;/b&gt; note"),
            "a note must be escaped as an ordinary text node: {html}"
        );
    }

    /// An empty note feed adds no notes markup at all.
    #[test]
    fn node_panel_with_no_notes_renders_no_notes_node() {
        let html = node_panel("n1", &[], &[], &[], 0).into_string();
        assert!(!html.contains("data-node=\"notes\""), "{html}");
    }

    /// The turn-history pane renders BELOW the asks/idle view with every
    /// source escaped as an ordinary text node (never `PreEscaped`) — a
    /// source containing `<script>` must not survive as live markup.
    #[test]
    fn node_panel_renders_turn_history_below_view_and_escaped() {
        let history = vec!["resume (Approve :: Decision) -- <script>alert(1)</script>".to_string()];
        let html = node_panel("n1", &[], &[], &history, 0).into_string();
        assert!(html.contains("<details"), "{html}");
        assert!(html.contains("Haskell turns (1)"), "{html}");
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        let idle_pos = html.find("Standby").expect("idle view still rendered");
        let details_pos = html.find("<details").expect("details rendered");
        assert!(
            idle_pos < details_pos,
            "the turn-history pane must render BELOW the asks/idle view:\n{html}"
        );
    }

    /// Every retained turn renders as its own entry, newest first, with only
    /// the NEWEST entry open — the operator scrolls back through the rest.
    #[test]
    fn node_panel_turn_history_lists_all_turns_newest_first_newest_open() {
        let history = vec![
            "pure (toJSON 1) -- oldest".to_string(),
            "pure (toJSON 2) -- middle".to_string(),
            "pure (toJSON 3) -- newest".to_string(),
        ];
        let html = node_panel("n1", &[], &[], &history, 0).into_string();
        assert!(html.contains("Haskell turns (3)"), "{html}");
        let p1 = html.find("turn 1 —").expect("oldest entry rendered");
        let p3 = html.find("turn 3 —").expect("newest entry rendered");
        assert!(p3 < p1, "newest must render first:\n{html}");
        let open_entries = html.matches("data-node=\"turn-entry\" open").count();
        assert_eq!(open_entries, 1, "only the newest entry is open: {html}");
        let open_pos = html.find("data-node=\"turn-entry\" open").unwrap();
        assert!(open_pos < p1, "the open entry is the newest:\n{html}");
    }

    /// An empty history adds no turn-source markup at all.
    #[test]
    fn node_panel_with_no_turn_history_renders_no_details() {
        let html = node_panel("n1", &[], &[], &[], 0).into_string();
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
