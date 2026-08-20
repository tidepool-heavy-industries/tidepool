//! One node's section markup — the minimal node-lifecycle model, rendered.
//!
//! A node is one append-only TIMELINE in a tree: seeds (a window opening
//! with its starting prompt), notes, asks (an answered ask stays in place
//! with its answer — it never vanishes), and outcomes (a finalized value or
//! a failure) all land as entries at their true chronological position.
//! One node can live several windows in sequence — the unified root does,
//! every turn — and each chapter reads seed → notes/asks → outcome, in
//! order. [`NodeView`]/[`TimelineEntry`] carry exactly that; [`node_panel`]
//! renders it. Nothing harness-specific appears in this schema — companion
//! drafts, tensions, gate verdicts all flow through it as content.
//!
//! [`node_panel`] always yields a single `id="panel-<node_id>"` element —
//! the unit the SSE stream patches in place, and the page shell embeds one
//! per registered node inside a stable `.node-slot` wrapper. The `data-rev`
//! attribute on that root is part of the wire contract — [`crate::shell`]'s
//! client JS compares it against the currently-mounted panel's to decide
//! whether a focus-preserving skip applies — and `data-path` carries the
//! node id so the client can MOUNT a panel it has never seen (a node born
//! after page load) into the tree at the right position. Each ask within
//! also carries its OWN `data-rev` (its interaction id, stable for its whole
//! lifetime).
//!
//! Every input carries a dotted `data-bind` path and a scalar `data-kind`.
//! The client collects those controls into a flat object and POSTs it to
//! `/node/<node>/submit/<interaction>` (baked directly into the rendered
//! `@post('...')` literal — no client-side URL assembly);
//! [`crate::server::collect_form_json`] validates and reassembles the object
//! using the same [`FormShape`] rendered here. Leaves bind at paths built by
//! `tidepool_harness::selfharness::operator::child_path`. `shell::JS`'s
//! collector treats a dotted path as an ordinary flat key; the server
//! reassembles nesting guided by the shape.
//!
//! A payload-bearing sum renders the discriminating choice and every
//! variant's nested payload form, all at once (server-rendered, no
//! client-side branching) — each variant's fields are bound under
//! `<path>.<Constructor>.<field>`, so different branches' fields never
//! collide even though they're all present in the DOM simultaneously.
//! `server::collect_form_json` reads the chosen constructor and only looks
//! at that branch's fields; a self-contained `<style>` block (CSS `:has()`)
//! visually hides every non-chosen branch.
//!
//! Model-authored text (seeds, notes, values, turn sources, failure reasons)
//! renders as maud-ESCAPED TEXT CONTENT only — never markup, never an
//! attribute value.

use std::collections::VecDeque;

use maud::{html, Markup, PreEscaped};
use serde_json::{Map, Value as Jv};
use tidepool_harness::selfharness::operator::{
    child_path, humanize_key, FieldShape, FormShape, VariantShape, ROOT_BIND_PATH,
};

/// One item in a node's chronological timeline: narration, an ask in one of
/// its two lifecycle states (pending: a live form; answered: kept in place,
/// read-only, with what the operator submitted), or a LIFECYCLE EVENT —
/// seeds, finalized values, and failures are timeline entries at their true
/// chronological position, not slots. One node can therefore live several
/// windows in sequence (the unified root does, every turn): each chapter
/// reads seed → notes/asks → outcome, in order.
pub enum TimelineEntry<'a> {
    /// Display-only narration (`note`), in post order with everything else.
    Note(&'a str),
    /// A window opened here with this starting prompt
    /// ([`OperatorGate::node_seeded`]).
    Seeded(&'a str),
    /// A window finalized here with this JSON-rendered answer
    /// ([`OperatorGate::node_finalized`]).
    Finalized(&'a str),
    /// A window ended here without an answer
    /// ([`OperatorGate::node_failed`]).
    Failed(&'a str),
    /// A live `askUser` form awaiting submission.
    PendingForm { id: u64, shape: &'a FormShape },
    /// The live between-turns gate awaiting the operator.
    PendingContinue { id: u64 },
    /// A form that was answered — stays at its position with the reassembled
    /// answer the harness actually received.
    AnsweredForm {
        id: u64,
        shape: &'a FormShape,
        answer: &'a Jv,
    },
    /// A continue gate that was clicked — with the operator's message, if
    /// they attached one.
    AnsweredContinue { id: u64, input: Option<&'a str> },
}

impl TimelineEntry<'_> {
    fn is_pending(&self) -> bool {
        matches!(
            self,
            TimelineEntry::PendingForm { .. } | TimelineEntry::PendingContinue { .. }
        )
    }
}

/// Everything [`node_panel`] renders for one node — the timeline (which now
/// carries the whole lifecycle) plus the render bookkeeping (`rev`, `done`,
/// the turn-source history pane).
pub struct NodeView<'a> {
    pub node_id: &'a str,
    pub timeline: Vec<TimelineEntry<'a>>,
    pub done: bool,
    pub turn_history: &'a VecDeque<String>,
    pub rev: u64,
}

/// A node's derived status — never stored, always a function of the view.
///
/// A pending ask outranks EVERYTHING, `done` included: the unified root node
/// is marked done at each turn's fold and then immediately carries the
/// between-turns gate — "do I need to act" is the one question the badge
/// answers, so operator-actionable always wins. Otherwise a live node is
/// running, and an ended node reports how its LAST window ended: the scan
/// walks the timeline backward to the most recent lifecycle marker, so a
/// revived node's earlier outcomes never speak for its current window.
fn status(view: &NodeView) -> (&'static str, &'static str) {
    if view.timeline.iter().any(TimelineEntry::is_pending) {
        return ("needs-you", "needs you");
    }
    if !view.done {
        return ("running", "running");
    }
    for entry in view.timeline.iter().rev() {
        match entry {
            TimelineEntry::Finalized(_) => return ("done", "done"),
            TimelineEntry::Failed(_) => return ("failed", "failed"),
            TimelineEntry::Seeded(_) => break,
            _ => {}
        }
    }
    ("ended", "ended")
}

/// The `id="panel-<node_id>"` fragment for one node — the one element the
/// SSE stream patches in place for that node. Rendered both into the initial
/// page ([`crate::shell::page`]) and into every SSE frame for `node_id`.
///
/// Anatomy, top to bottom: header (full path + collapse toggle + status
/// badge) → the timeline in true chronological order (seeds as collapsed
/// `<details>` — briefs are long; pending asks STACKED and live; answered
/// asks read-only in place; finalized values and failures where they
/// happened) → the turn-source history.
pub fn node_panel(view: &NodeView) -> Markup {
    let (status_class, status_label) = status(view);
    html! {
        div id=(panel_id(view.node_id)) data-rev=(view.rev) data-path=(view.node_id)
            class=(format!("node-panel {status_class}")) {
            header class="node-head" {
                button type="button" class="node-toggle" data-toggle=(view.node_id)
                    aria-label="collapse" { "▾" }
                h2 class="node-title" title=(view.node_id) { (truncate_title(view.node_id)) }
                span class=(format!("status {status_class}")) { (status_label) }
            }
            div class="node-body" {
                @if !view.timeline.is_empty() {
                    div class="timeline" data-node="timeline" {
                        @for entry in &view.timeline {
                            (timeline_entry(view.node_id, entry))
                        }
                    }
                } @else if !view.done {
                    (idle())
                }
                @if !view.turn_history.is_empty() {
                    (turn_history_pane(view.turn_history))
                }
            }
        }
    }
}

/// The DOM id a node's panel root carries — `id="panel-<node_id>"`.
#[must_use]
pub fn panel_id(node_id: &str) -> String {
    format!("panel-{node_id}")
}

/// Re-render a JSON text pretty-printed when it parses, verbatim when it
/// doesn't (a finalized value is always JSON today, but a non-JSON string
/// must still display rather than vanish).
fn pretty_json(value: &str) -> String {
    serde_json::from_str::<Jv>(value)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| value.to_string())
}

/// The character budget a node section's title truncates to — long enough
/// to show a labeled branch's discriminating suffix, short enough that a
/// deep tree still reads as one outline. See [`truncate_title`].
const TITLE_BUDGET: usize = 40;

/// Truncate `label` to at most [`TITLE_BUDGET`] characters at the LAST word
/// boundary within budget, never mid-word, appending an ellipsis. A tree-path
/// node id carries no spaces, so `/`, `-`, and `_` all count as boundaries
/// alongside literal whitespace. Falls back to a hard cut at budget only when
/// no boundary exists at all (a single very long word). A label already
/// within budget is returned unchanged, with no ellipsis — the full label
/// always rides alongside it as the caller's `title` attribute.
fn truncate_title(label: &str) -> String {
    if label.chars().count() <= TITLE_BUDGET {
        return label.to_string();
    }
    let head: String = label.chars().take(TITLE_BUDGET).collect();
    let cut = head.rfind(['/', '-', '_', ' ']).unwrap_or(head.len());
    let mut kept = head[..cut].to_string();
    if kept.is_empty() {
        kept = head;
    }
    kept.push('…');
    kept
}

/// Render a finalized/failure value STRUCTURED when it parses as a JSON
/// object — a definition list, one row per field, exactly the shape a
/// `ProposeFinish`/`FoldDecision`-style answer takes — falling back to the
/// original pretty-printed `<pre>` for anything else (a bare scalar, an
/// array, or text that isn't JSON at all). Every field value is still an
/// ordinary maud-escaped text node; nothing here renders model text as
/// markup.
fn structured_value(value: &str) -> Markup {
    match serde_json::from_str::<Jv>(value) {
        Ok(Jv::Object(fields)) => structured_object(&fields),
        _ => html! { pre { (pretty_json(value)) } },
    }
}

/// A JSON object as a definition list: a `_con`/`tag` field (the generic
/// sum-type discriminant this codebase's two JSON encodings use) renders as
/// a small badge instead of an ordinary field row; every other field is a
/// [`humanize_key`]'d label row whose value renders via
/// [`structured_field`].
fn structured_object(fields: &Map<String, Jv>) -> Markup {
    let con_key = ["_con", "tag"]
        .into_iter()
        .find(|k| matches!(fields.get(*k), Some(Jv::String(_))));
    html! {
        @if let Some(key) = con_key {
            @if let Some(Jv::String(con)) = fields.get(key) {
                span class="con-badge" { (con) }
            }
        }
        dl class="structured" {
            @for (key, value) in fields {
                @if Some(key.as_str()) != con_key {
                    div class="structured-row" {
                        dt class="eyebrow" { (humanize_key(key)) }
                        dd { (structured_field(value)) }
                    }
                }
            }
        }
    }
}

/// One field's value: a string as wrapped prose (the common case — a
/// paragraph of model-authored text, unreadable squeezed into a JSON blob),
/// an object/array recursively via the same treatment, any other scalar as
/// plain text.
fn structured_field(value: &Jv) -> Markup {
    match value {
        Jv::String(text) => html! { p class="prose" style="white-space: pre-wrap" { (text) } },
        Jv::Object(fields) => structured_object(fields),
        Jv::Array(items) => html! {
            div class="structured-list" {
                @for item in items {
                    (structured_field(item))
                }
            }
        },
        Jv::Null => html! { span class="scalar" { "—" } },
        Jv::Bool(b) => html! { span class="scalar" { (b.to_string()) } },
        Jv::Number(n) => html! { span class="scalar" { (n.to_string()) } },
    }
}

/// One timeline item, dispatched by kind.
fn timeline_entry(node_id: &str, entry: &TimelineEntry) -> Markup {
    match entry {
        TimelineEntry::Note(text) => html! {
            p class="note" style="white-space: pre-wrap" { (text) }
        },
        TimelineEntry::Seeded(seed) => html! {
            details class="seed" data-node="seed" {
                summary { "seed — " (snippet(seed)) }
                pre { (seed) }
            }
        },
        TimelineEntry::Finalized(value) => html! {
            div class="final" data-node="final" {
                p class="eyebrow" { "Final value" }
                (structured_value(value))
            }
        },
        TimelineEntry::Failed(reason) => html! {
            div class="failure" data-node="failure" {
                p class="eyebrow failure-eyebrow" { "Ended without a value" }
                (structured_value(reason))
            }
        },
        TimelineEntry::PendingForm { id, shape } => ask_form(node_id, *id, shape),
        TimelineEntry::PendingContinue { id } => ask_continue(node_id, *id),
        TimelineEntry::AnsweredForm { id, shape, answer } => {
            answered_form(node_id, *id, shape, answer)
        }
        TimelineEntry::AnsweredContinue { id, input } => answered_continue(node_id, *id, *input),
    }
}

/// An answered form, read-only at its original timeline position: the form's
/// type name and the answer the harness actually received (the reassembled
/// submission — the truth of what crossed the gate, not the raw wire).
fn answered_form(node_id: &str, interaction: u64, shape: &FormShape, answer: &Jv) -> Markup {
    html! {
        div id=(ask_id(node_id, interaction)) data-rev=(interaction)
            class="answered" data-node="answered" {
            p class="eyebrow" { "ask #" (interaction) " — Answered — " (shape_title(shape)) }
            pre { (answer_text(answer)) }
        }
    }
}

/// A clicked continue gate, read-only at its position.
fn answered_continue(node_id: &str, interaction: u64, input: Option<&str>) -> Markup {
    html! {
        div id=(ask_id(node_id, interaction)) data-rev=(interaction)
            class="answered" data-node="answered" {
            @match input {
                Some(text) => {
                    p class="eyebrow" { "ask #" (interaction) " — Continued, with input" }
                    pre { (text) }
                }
                None => {
                    p class="eyebrow" { "ask #" (interaction) " — Continued" }
                }
            }
        }
    }
}

/// The form's display title — its answer type's own name where the shape
/// carries one, a generic word where it doesn't (leaves).
fn shape_title(shape: &FormShape) -> &str {
    match shape {
        FormShape::Product { type_key, .. } | FormShape::Sum { type_key, .. } => type_key,
        _ => "answer",
    }
}

/// An answer value as display text: bare text for a string (the common
/// free-text reply reads as prose, not as a quoted JSON literal), pretty
/// JSON for anything structured.
fn answer_text(answer: &Jv) -> String {
    match answer {
        Jv::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// The turn-history pane, at the bottom of the node body: one nested
/// `<details>` per compiled turn, NEWEST FIRST with only the newest open.
/// Entries are numbered in post order. Plain `<pre>`, every source string an
/// ordinary maud text node (escaped), never `PreEscaped`.
fn turn_history_pane(history: &VecDeque<String>) -> Markup {
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

/// A one-line teaser: the text's first line, truncated. Rendered as a text
/// node like everything else.
fn snippet(source: &str) -> String {
    let head = source.lines().next().unwrap_or("").trim();
    let mut s: String = head.chars().take(64).collect();
    if head.chars().count() > 64 {
        s.push('…');
    }
    s
}

/// Idle placeholder — a live node with nothing to show yet.
fn idle() -> Markup {
    html! {
        div class="idle" {
            p class="eyebrow" { "Standby" }
            p class="idle-glyph" { "—" }
            p class="idle-note" { "No operator input pending. The harness is thinking." }
        }
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
            p class="eyebrow ask-label" { "ask #" (interaction) }
            @if let Some(doc) = shape_doc(shape) {
                p class="form-intro" data-node="form-intro" { (doc) }
            }
            (generic_shape(ROOT_BIND_PATH, shape))
            div class="actions" {
                button type="submit" class="btn btn-primary" { "Submit" }
            }
        }
    }
}

/// A [`FormShape::Product`]/[`FormShape::Sum`]'s own `doc`, if it carries
/// one — on the ROOT shape, the form's title/intro prose; on a `Sum`
/// variant's payload shape, that variant's payload doc.
fn shape_doc(shape: &FormShape) -> Option<&str> {
    match shape {
        FormShape::Product { doc, .. } | FormShape::Sum { doc, .. } => doc.as_deref(),
        _ => None,
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
                    doc: None,
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
                        doc: None,
                    }],
                    doc: None,
                },
            },
        ],
        doc: None,
    }
}

/// One pending between-loops continue gate: the [`continue_shape`] sum
/// rendered by the generic machinery, POSTing to
/// `/node/<node_id>/continue/<interaction>`.
fn ask_continue(node_id: &str, interaction: u64) -> Markup {
    html! {
        form id=(ask_id(node_id, interaction)) data-rev=(interaction) class="continue"
             data-on-submit=(post_url(node_id, "continue", interaction)) {
            p class="eyebrow" { "ask #" (interaction) " — Turn complete — start the next turn?" }
            (generic_shape(ROOT_BIND_PATH, &continue_shape()))
            button type="submit" class="btn btn-primary" { "Start next turn" }
        }
    }
}

/// The DOM id one ask carries — `id="ask-<node_id>-<interaction>"` — for its
/// whole lifetime, pending and answered alike.
fn ask_id(node_id: &str, interaction: u64) -> String {
    format!("ask-{node_id}-{interaction}")
}

/// The literal `@post('...')` target `shell::JS`'s vendored client parses out
/// of a `data-on-submit` attribute — node- and interaction-scoped, baked in
/// at render time so no client-side URL assembly is needed.
///
/// The node id is percent-encoded into ONE path segment: tree paths carry
/// literal slashes (`root/1-x`), and the axum route `/node/{node}/...`
/// matches `{node}` as a single segment — a raw slash in the baked URL 404s
/// before any handler runs (found live by the zero-context operator probe,
/// 2026-08-19; axum percent-decodes the matched segment back to the id).
fn post_url(node_id: &str, verb: &str, interaction: u64) -> String {
    format!(
        "@post('/node/{}/{verb}/{interaction}')",
        encode_path_segment(node_id)
    )
}

/// Percent-encode `s` as a single URL path segment: every byte outside the
/// RFC 3986 unreserved set (`A-Z a-z 0-9 - . _ ~`) is `%XX`-escaped —
/// slashes included, which is the whole point.
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
            @if let Some(doc) = &field.doc {
                p class="field-help" data-node="field-help" { (doc) }
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
                            @if let Some(doc) = shape_doc(&v.shape) {
                                p class="field-help" data-node="field-help" { (doc) }
                            }
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
    use serde_json::json;

    fn empty_history() -> VecDeque<String> {
        VecDeque::new()
    }

    fn base_view<'a>(
        node_id: &'a str,
        timeline: Vec<TimelineEntry<'a>>,
        turn_history: &'a VecDeque<String>,
        rev: u64,
    ) -> NodeView<'a> {
        NodeView {
            node_id,
            timeline,
            done: false,
            turn_history,
            rev,
        }
    }

    #[test]
    fn node_panel_continue_renders_button() {
        let th = empty_history();
        let view = base_view("n1", vec![TimelineEntry::PendingContinue { id: 0 }], &th, 0);
        let html = node_panel(&view).into_string();
        assert!(html.contains("@post('/node/n1/continue/0')"));
    }

    #[test]
    fn node_panel_idle_is_quiet() {
        let th = empty_history();
        let html = node_panel(&base_view("n1", vec![], &th, 0)).into_string();
        assert!(html.starts_with("<div id=\"panel-n1\""));
        assert!(!html.contains("@post"));
        assert!(html.contains("Standby"), "{html}");
    }

    /// F10 (per-node): `node_panel()` stamps the view's revision as
    /// `data-rev` on the panel root, and a different revision produces a
    /// different attribute value. `data-path` rides alongside so the client
    /// can mount a never-seen panel into the tree.
    #[test]
    fn node_panel_stamps_data_rev_and_data_path() {
        let th = empty_history();
        let a = node_panel(&base_view("n1", vec![], &th, 7)).into_string();
        assert!(
            a.contains("id=\"panel-n1\" data-rev=\"7\" data-path=\"n1\""),
            "{a}"
        );

        let b = node_panel(&base_view("n1", vec![], &th, 8)).into_string();
        assert!(b.contains("id=\"panel-n1\" data-rev=\"8\""), "{b}");
        assert_ne!(a, b);
    }

    /// The panel id is node-scoped, distinguishing sections.
    #[test]
    fn node_panel_id_is_scoped_per_node() {
        let th = empty_history();
        let a = node_panel(&base_view("alpha", vec![], &th, 0)).into_string();
        let b = node_panel(&base_view("beta", vec![], &th, 0)).into_string();
        assert!(a.starts_with("<div id=\"panel-alpha\""));
        assert!(b.starts_with("<div id=\"panel-beta\""));
    }

    /// The header carries the FULL path as the title (depth also indents via
    /// the shell wrapper), a collapse toggle, and the derived status badge.
    #[test]
    fn node_panel_header_has_full_path_toggle_and_status() {
        let th = empty_history();
        let html = node_panel(&base_view("root/1-x", vec![], &th, 0)).into_string();
        assert!(html.contains(">root/1-x</h2>"), "{html}");
        assert!(html.contains("data-toggle=\"root/1-x\""), "{html}");
        assert!(html.contains(">running</span>"), "{html}");
    }

    /// A short node id renders untouched in the title, with no ellipsis.
    #[test]
    fn node_title_short_label_is_untouched() {
        let th = empty_history();
        let html = node_panel(&base_view("root/1-x", vec![], &th, 0)).into_string();
        assert!(html.contains(">root/1-x</h2>"), "{html}");
        assert!(!html.contains('…'), "{html}");
        assert!(html.contains("title=\"root/1-x\""), "{html}");
    }

    /// A long node id truncates at the LAST word boundary within budget
    /// (never mid-word), with the FULL id carried in the `title` attribute
    /// for hover. Pins the paper cut this fixes: a naive char-count cut used
    /// to land mid-word ("...operator-interru").
    #[test]
    fn node_title_long_label_truncates_at_word_boundary_with_full_title_attr() {
        let th = empty_history();
        let long_id = "root/2-forms-notes-and-operator-interruptions-that-need-review";
        let expected = truncate_title(long_id);
        assert!(expected.ends_with('…'), "{expected}");
        assert!(
            !expected.contains("interru"),
            "must not cut mid-word: {expected}"
        );

        let html = node_panel(&base_view(long_id, vec![], &th, 0)).into_string();
        assert!(html.contains(&format!(">{expected}</h2>")), "{html}");
        assert!(html.contains(&format!("title=\"{long_id}\"")), "{html}");
    }

    /// Status derivation over the lifecycle-in-timeline model. A pending ask
    /// outranks `done` (the unified root is done at every fold while its
    /// between-turns gate is pending — the badge answers "do I need to
    /// act"); an ended node reports its LAST window's outcome, so a revived
    /// node's earlier chapters never speak for the current one.
    #[test]
    fn node_panel_status_derives_from_the_view() {
        let th = empty_history();
        let shape = FormShape::String;

        let needs = node_panel(&base_view(
            "n",
            vec![TimelineEntry::PendingForm {
                id: 0,
                shape: &shape,
            }],
            &th,
            0,
        ))
        .into_string();
        assert!(needs.contains(">needs you</span>"), "{needs}");

        // Pending outranks done: a done node with a live gate needs you.
        let mut parked = base_view(
            "n",
            vec![
                TimelineEntry::Finalized("{\"ok\":true}"),
                TimelineEntry::PendingContinue { id: 1 },
            ],
            &th,
            0,
        );
        parked.done = true;
        let parked = node_panel(&parked).into_string();
        assert!(parked.contains(">needs you</span>"), "{parked}");

        let mut done = base_view("n", vec![TimelineEntry::Finalized("{\"ok\":true}")], &th, 0);
        done.done = true;
        let done = node_panel(&done).into_string();
        assert!(done.contains(">done</span>"), "{done}");

        let mut ended = base_view("n", vec![], &th, 0);
        ended.done = true;
        let ended = node_panel(&ended).into_string();
        assert!(ended.contains(">ended</span>"), "{ended}");

        let mut failed = base_view("n", vec![TimelineEntry::Failed("round exhaustion")], &th, 0);
        failed.done = true;
        let failed = node_panel(&failed).into_string();
        assert!(failed.contains(">failed</span>"), "{failed}");

        // A revived node's NEW window outranks the old chapter's outcome:
        // [Finalized (turn 1), Seeded (turn 2)] + done = ended, not done.
        let mut revived = base_view(
            "n",
            vec![
                TimelineEntry::Finalized("{\"ok\":true}"),
                TimelineEntry::Seeded("next chapter"),
            ],
            &th,
            0,
        );
        revived.done = true;
        let revived = node_panel(&revived).into_string();
        assert!(revived.contains(">ended</span>"), "{revived}");
    }

    /// The seed renders as a collapsed details block with a one-line teaser,
    /// escaped as ordinary text — inline in the timeline, at its position.
    #[test]
    fn node_panel_renders_seed_collapsed_and_escaped() {
        let th = empty_history();
        let view = base_view(
            "n1",
            vec![TimelineEntry::Seeded(
                "NODE root/1 — DISCOVER <script>alert(1)</script>",
            )],
            &th,
            0,
        );
        let html = node_panel(&view).into_string();
        assert!(html.contains("data-node=\"seed\""), "{html}");
        assert!(
            !html.contains("data-node=\"seed\" open"),
            "seed starts collapsed: {html}"
        );
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    /// A finalized value that parses as a JSON OBJECT renders structured: a
    /// `tag`/`_con` field becomes a small badge, every other field becomes a
    /// humanized-label row — never a raw `<pre>` JSON blob.
    #[test]
    fn node_panel_renders_final_value_structured_for_an_object() {
        let th = empty_history();
        let mut view = base_view(
            "n1",
            vec![TimelineEntry::Finalized(
                "{\"tag\":\"FinishLayer\",\"confidence\":\"High\"}",
            )],
            &th,
            0,
        );
        view.done = true;
        let html = node_panel(&view).into_string();
        assert!(html.contains("data-node=\"final\""), "{html}");
        assert!(html.contains("Final value"), "{html}");
        assert!(html.contains("class=\"con-badge\""), "{html}");
        assert!(html.contains("FinishLayer"), "{html}");
        assert!(html.contains("Confidence"), "humanized label: {html}");
        assert!(html.contains("High"), "{html}");
        assert!(
            !html.contains("&quot;tag&quot;"),
            "no raw JSON blob: {html}"
        );
    }

    /// The design target this rendering exists for: a multi-paragraph
    /// `ProposeFinish`-shaped answer reads as prose paragraphs, not one
    /// escaped JSON string in a `<pre>`. Field values stay escaped text
    /// nodes throughout.
    #[test]
    fn node_panel_renders_multi_paragraph_finalized_value_as_prose() {
        let th = empty_history();
        let value = serde_json::json!({
            "_con": "ProposeFinish",
            "localAnswer": "Paragraph one, the summary.\n\nParagraph two, the detail.",
            "confidence": "High",
        })
        .to_string();
        let mut view = base_view("n1", vec![TimelineEntry::Finalized(&value)], &th, 0);
        view.done = true;
        let html = node_panel(&view).into_string();

        assert!(html.contains("class=\"con-badge\""), "{html}");
        assert!(html.contains("ProposeFinish"), "{html}");
        assert!(html.contains("Local answer"), "humanized label: {html}");
        assert!(html.contains("class=\"prose\""), "{html}");
        assert!(
            html.contains("Paragraph one, the summary."),
            "the prose itself renders: {html}"
        );
        // No raw JSON braces/quoting leak into the rendered markup.
        assert!(!html.contains("{&quot;"), "{html}");
        assert!(!html.contains("\\n\\n"), "{html}");
    }

    /// Field values inside a structured finalized object are still ordinary
    /// maud-escaped text nodes — the injection rule holds for the new
    /// rendering path exactly as it does everywhere else in this crate.
    #[test]
    fn node_panel_structured_final_value_escapes_field_text() {
        let th = empty_history();
        let value = "{\"note\":\"<script>alert(1)</script>\"}";
        let mut view = base_view("n1", vec![TimelineEntry::Finalized(value)], &th, 0);
        view.done = true;
        let html = node_panel(&view).into_string();
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    /// A finalized value that does NOT parse as a JSON object (a bare
    /// scalar, or non-JSON text) keeps today's pretty-printed `<pre>`
    /// fallback rather than vanishing or erroring.
    #[test]
    fn node_panel_non_object_final_value_falls_back_to_pre() {
        let th = empty_history();
        let mut view = base_view(
            "n1",
            vec![TimelineEntry::Finalized("\"turn 1 answer\"")],
            &th,
            0,
        );
        view.done = true;
        let html = node_panel(&view).into_string();
        assert!(html.contains("<pre>"), "{html}");
        assert!(html.contains("turn 1 answer"), "{html}");
    }

    /// The same structured treatment applies to a Failed reason block when
    /// it parses as JSON.
    #[test]
    fn node_panel_renders_failure_structured_for_an_object() {
        let th = empty_history();
        let mut view = base_view(
            "n1",
            vec![TimelineEntry::Failed(
                "{\"tag\":\"RoundExhaustion\",\"detail\":\"8 rounds without finalize\"}",
            )],
            &th,
            0,
        );
        view.done = true;
        let html = node_panel(&view).into_string();
        assert!(html.contains("data-node=\"failure\""), "{html}");
        assert!(html.contains("class=\"con-badge\""), "{html}");
        assert!(html.contains("RoundExhaustion"), "{html}");
        assert!(html.contains("8 rounds without finalize"), "{html}");
    }

    /// A failure renders its reason as escaped text under a distinct block.
    #[test]
    fn node_panel_renders_failure_reason() {
        let th = empty_history();
        let mut view = base_view(
            "n1",
            vec![TimelineEntry::Failed(
                "round exhaustion — 8 rounds without finalize",
            )],
            &th,
            0,
        );
        view.done = true;
        let html = node_panel(&view).into_string();
        assert!(html.contains("data-node=\"failure\""), "{html}");
        assert!(html.contains("Ended without a value"), "{html}");
        assert!(html.contains("round exhaustion"), "{html}");
    }

    /// A multi-chapter timeline (the unified root across turns) renders each
    /// chapter's events at their true positions: seed → note → final value →
    /// answered gate → next seed.
    #[test]
    fn node_panel_renders_chapters_in_order() {
        let th = empty_history();
        let view = base_view(
            "root",
            vec![
                TimelineEntry::Seeded("turn 1 brief"),
                TimelineEntry::Note("working"),
                TimelineEntry::Finalized("\"turn 1 answer\""),
                TimelineEntry::AnsweredContinue {
                    id: 0,
                    input: Some("steer"),
                },
                TimelineEntry::Seeded("turn 2 brief"),
            ],
            &th,
            0,
        );
        let html = node_panel(&view).into_string();
        let p = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        assert!(
            p("turn 1 brief") < p("working")
                && p("working") < p("turn 1 answer")
                && p("turn 1 answer") < p("steer")
                && p("steer") < p("turn 2 brief"),
            "{html}"
        );
    }

    /// The core timeline behavior: entries render in true chronological
    /// order — a note, an ANSWERED ask (read-only, with its answer), then a
    /// pending ask — and the answered ask never renders form controls.
    #[test]
    fn node_panel_timeline_keeps_chronology_and_answered_asks() {
        let th = empty_history();
        let shape = FormShape::String;
        let answer = json!("keep going");
        let view = base_view(
            "n1",
            vec![
                TimelineEntry::Note("about to ask"),
                TimelineEntry::AnsweredForm {
                    id: 3,
                    shape: &shape,
                    answer: &answer,
                },
                TimelineEntry::PendingForm {
                    id: 7,
                    shape: &shape,
                },
            ],
            &th,
            42,
        );
        let html = node_panel(&view).into_string();

        let note_pos = html.find("about to ask").expect("note rendered");
        let answered_pos = html.find("id=\"ask-n1-3\"").expect("answered ask rendered");
        let pending_pos = html.find("id=\"ask-n1-7\"").expect("pending ask rendered");
        assert!(
            note_pos < answered_pos && answered_pos < pending_pos,
            "{html}"
        );

        assert!(
            html.contains("keep going"),
            "the submitted answer shows: {html}"
        );
        assert!(html.contains("@post('/node/n1/submit/7')"), "{html}");
        assert!(
            !html.contains("@post('/node/n1/submit/3')"),
            "an answered ask has no live form: {html}"
        );
        assert!(html.contains("id=\"panel-n1\" data-rev=\"42\""), "{html}");
    }

    /// Two pending asks on one node BOTH render, each with its own stable
    /// id/data-rev keyed on its interaction id — an operator gate never
    /// hides a question by only showing the newest.
    #[test]
    fn node_panel_stacks_every_pending_ask_with_its_own_rev() {
        let th = empty_history();
        let shape = FormShape::String;
        let view = base_view(
            "n1",
            vec![
                TimelineEntry::PendingForm {
                    id: 3,
                    shape: &shape,
                },
                TimelineEntry::PendingContinue { id: 7 },
            ],
            &th,
            42,
        );
        let html = node_panel(&view).into_string();

        assert!(html.contains("id=\"ask-n1-3\" data-rev=\"3\""), "{html}");
        assert!(html.contains("id=\"ask-n1-7\" data-rev=\"7\""), "{html}");
        assert!(html.contains("@post('/node/n1/submit/3')"), "{html}");
        assert!(html.contains("@post('/node/n1/continue/7')"), "{html}");
    }

    /// A stacked pair of pending asks each carries a visible `ask #<id>`
    /// identity label — the paper cut a zero-context operator hit: with no
    /// label, two independent live questions are indistinguishable from a
    /// stale form the page failed to clean up.
    #[test]
    fn stacked_pending_asks_each_show_a_visible_ask_id_label() {
        let th = empty_history();
        let shape = FormShape::String;
        let view = base_view(
            "n1",
            vec![
                TimelineEntry::PendingForm {
                    id: 3,
                    shape: &shape,
                },
                TimelineEntry::PendingContinue { id: 7 },
            ],
            &th,
            42,
        );
        let html = node_panel(&view).into_string();

        assert!(html.contains("ask #3"), "{html}");
        assert!(html.contains("ask #7"), "{html}");
    }

    /// A tree-path node id (containing literal slashes) bakes a
    /// percent-encoded `@post` target: the axum route matches `{node}` as a
    /// SINGLE segment, so a raw slash 404s before any handler runs — found
    /// live by the zero-context operator probe (2026-08-19), which proved
    /// the `%2F` form resolves correctly.
    #[test]
    fn post_urls_percent_encode_slash_path_node_ids() {
        let th = empty_history();
        let view = base_view(
            "root/1-x",
            vec![
                TimelineEntry::PendingForm {
                    id: 0,
                    shape: &FormShape::String,
                },
                TimelineEntry::PendingContinue { id: 1 },
            ],
            &th,
            0,
        );
        let html = node_panel(&view).into_string();
        assert!(
            html.contains("@post('/node/root%2F1-x/submit/0')"),
            "{html}"
        );
        assert!(
            html.contains("@post('/node/root%2F1-x/continue/1')"),
            "{html}"
        );
        assert!(
            !html.contains("@post('/node/root/1-x"),
            "no raw-slash target may survive: {html}"
        );
    }

    /// An answered continue renders the operator's message when one was
    /// attached, and a plain marker when not.
    #[test]
    fn node_panel_renders_answered_continue_with_and_without_input() {
        let th = empty_history();
        let with = base_view(
            "n1",
            vec![TimelineEntry::AnsweredContinue {
                id: 1,
                input: Some("focus on receipts"),
            }],
            &th,
            0,
        );
        let html = node_panel(&with).into_string();
        assert!(html.contains("Continued, with input"), "{html}");
        assert!(html.contains("focus on receipts"), "{html}");

        let without = base_view(
            "n1",
            vec![TimelineEntry::AnsweredContinue { id: 1, input: None }],
            &th,
            0,
        );
        let html = node_panel(&without).into_string();
        assert!(html.contains("Continued"), "{html}");
    }

    /// Notes are escaped as ordinary text nodes.
    #[test]
    fn node_panel_escapes_notes() {
        let th = empty_history();
        let view = base_view(
            "n1",
            vec![TimelineEntry::Note("<b>second</b> note")],
            &th,
            0,
        );
        let html = node_panel(&view).into_string();
        assert!(
            html.contains("&lt;b&gt;second&lt;/b&gt; note"),
            "a note must be escaped as an ordinary text node: {html}"
        );
    }

    /// The turn-history pane renders at the BOTTOM of the node body with
    /// every source escaped as an ordinary text node (never `PreEscaped`).
    #[test]
    fn node_panel_renders_turn_history_below_and_escaped() {
        let history = VecDeque::from([
            "resume (Approve :: Decision) -- <script>alert(1)</script>".to_string()
        ]);
        let view = base_view("n1", vec![TimelineEntry::Note("a note")], &history, 0);
        let html = node_panel(&view).into_string();
        assert!(html.contains("Haskell turns (1)"), "{html}");
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        let note_pos = html.find("a note").expect("note rendered");
        let details_pos = html
            .find("data-node=\"turn-source\"")
            .expect("pane rendered");
        assert!(
            note_pos < details_pos,
            "the turn-history pane renders below the timeline:\n{html}"
        );
    }

    /// Every retained turn renders as its own entry, newest first, with only
    /// the NEWEST entry open.
    #[test]
    fn node_panel_turn_history_lists_all_turns_newest_first_newest_open() {
        let history = VecDeque::from([
            "pure (toJSON 1) -- oldest".to_string(),
            "pure (toJSON 2) -- middle".to_string(),
            "pure (toJSON 3) -- newest".to_string(),
        ]);
        let view = base_view("n1", vec![], &history, 0);
        let html = node_panel(&view).into_string();
        assert!(html.contains("Haskell turns (3)"), "{html}");
        let p1 = html.find("turn 1 —").expect("oldest entry rendered");
        let p3 = html.find("turn 3 —").expect("newest entry rendered");
        assert!(p3 < p1, "newest must render first:\n{html}");
        let open_entries = html.matches("data-node=\"turn-entry\" open").count();
        assert_eq!(open_entries, 1, "only the newest entry is open: {html}");
        let open_pos = html.find("data-node=\"turn-entry\" open").unwrap();
        assert!(open_pos < p1, "the open entry is the newest:\n{html}");
    }

    // ---- generic_shape ------------------------------------------------------

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
            doc: None,
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
                                doc: None,
                            },
                            FieldShape {
                                key: "port".to_string(),
                                shape: FormShape::Int,
                                doc: None,
                            },
                        ],
                        doc: None,
                    },
                },
            ],
            doc: None,
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
                    doc: None,
                },
                FieldShape {
                    key: "environment".to_string(),
                    shape: FormShape::Sum {
                        type_key: "Environment".to_string(),
                        variants: vec![VariantShape {
                            constructor: "NeedsReview".to_string(),
                            shape: empty_product("Environment", "NeedsReview"),
                        }],
                        doc: None,
                    },
                    doc: None,
                },
            ],
            doc: None,
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
            doc: None,
        }
    }

    // ---- doc rendering --------------------------------------------------

    /// A root shape's `doc` renders as visible intro prose above the form's
    /// fields.
    #[test]
    fn root_doc_renders_as_visible_intro_prose() {
        let shape = FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![],
            doc: Some("Configure the SSH connection.".to_string()),
        };
        let html = ask_form("n1", 0, &shape).into_string();
        assert!(html.contains("Configure the SSH connection."), "{html}");
        assert!(html.contains("data-node=\"form-intro\""), "{html}");
    }

    /// Model-authored doc text is a maud-escaped text node, same as every
    /// other model-authored string this crate renders.
    #[test]
    fn root_doc_is_escaped() {
        let shape = FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![],
            doc: Some("<script>alert(1)</script>".to_string()),
        };
        let html = ask_form("n1", 0, &shape).into_string();
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    /// A field's `doc` renders as help text between the humanized label and
    /// the input control.
    #[test]
    fn field_doc_renders_between_label_and_control() {
        let shape = FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![FieldShape {
                key: "host".to_string(),
                shape: FormShape::String,
                doc: Some("The hostname to connect to.".to_string()),
            }],
            doc: None,
        };
        let html = generic_shape("", &shape).into_string();
        assert!(html.contains("The hostname to connect to."), "{html}");
        assert!(html.contains("data-node=\"field-help\""), "{html}");

        let label_pos = html.find("Host").expect("humanized label rendered");
        let help_pos = html
            .find("The hostname to connect to.")
            .expect("help text rendered");
        let control_pos = html
            .find("data-bind=\"host\"")
            .expect("input control rendered");
        assert!(
            label_pos < help_pos && help_pos < control_pos,
            "help text must render between the label and the control: {html}"
        );
    }

    /// Field doc text is escaped, same as root doc text.
    #[test]
    fn field_doc_is_escaped() {
        let shape = FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![FieldShape {
                key: "host".to_string(),
                shape: FormShape::String,
                doc: Some("<b>hostname</b>".to_string()),
            }],
            doc: None,
        };
        let html = generic_shape("", &shape).into_string();
        assert!(!html.contains("<b>hostname</b>"), "{html}");
        assert!(html.contains("&lt;b&gt;hostname&lt;/b&gt;"), "{html}");
    }

    /// A Sum variant's payload doc renders inside that variant's own payload
    /// block.
    #[test]
    fn variant_payload_doc_renders_inside_its_block() {
        let shape = FormShape::Sum {
            type_key: "Destination".to_string(),
            variants: vec![VariantShape {
                constructor: "Ssh".to_string(),
                shape: FormShape::Product {
                    type_key: "Ssh".to_string(),
                    constructor: "Ssh".to_string(),
                    fields: vec![FieldShape {
                        key: "host".to_string(),
                        shape: FormShape::String,
                        doc: None,
                    }],
                    doc: Some("Connect over SSH.".to_string()),
                },
            }],
            doc: None,
        };
        let html = generic_shape("destination", &shape).into_string();
        let payload_pos = html
            .find("data-for=\"Ssh\"")
            .expect("payload block rendered");
        let doc_pos = html
            .find("Connect over SSH.")
            .expect("variant payload doc rendered");
        assert!(
            payload_pos < doc_pos,
            "the variant doc must render inside its own payload block: {html}"
        );
    }

    /// A docless form (no root doc, no field docs) renders with no help
    /// markup at all.
    #[test]
    fn docless_form_renders_with_no_help_markup() {
        let html = ask_form("n1", 0, &ssh_product_shape()).into_string();
        assert!(!html.contains("data-node=\"form-intro\""), "{html}");
        assert!(!html.contains("data-node=\"field-help\""), "{html}");
    }

    fn ssh_product_shape() -> FormShape {
        FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![
                FieldShape {
                    key: "host".to_string(),
                    shape: FormShape::String,
                    doc: None,
                },
                FieldShape {
                    key: "port".to_string(),
                    shape: FormShape::Int,
                    doc: None,
                },
            ],
            doc: None,
        }
    }
}
