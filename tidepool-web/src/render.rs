//! `Ui` → Datastar-ready HTML fragment renderer.
//!
//! Every function here is a pure function of a `&Ui` value (plus an
//! `answer_url` base — the harness's answer verbs don't exist yet, segment
//! 30 C4 wires the real endpoints in, so callers pass whatever base they
//! have). No state lives here: nothing is cached, nothing is mutated,
//! nothing is read back out of the `Ui` tree between renders.
//!
//! Trust model: a `Ui` value is constructed by the harness's Haskell
//! programs, but its CONTENTS (`Prose` text, `Choice` option keys/labels,
//! ...) can originate from the calling MODEL via `dialogAsk` — and a
//! prompt-injected model is an untrusted-input carrier. Loopback binding
//! (see `tidepool-web/CLAUDE.md`) stops a remote network attacker; it does
//! nothing about a payload the model was induced to emit and the operator's
//! own browser then executes same-origin, where e.g. `/eval_in_binding` is
//! arbitrary code against a live heap. What actually closes the surface:
//! `render_markdown` neutralizes raw HTML at the source (`Event::Html`/
//! `Event::InlineHtml` become escaped text, never live DOM) and every
//! model-supplied key interpolated into a single-quoted `@post('...')`
//! target is percent-encoded first (see `choice_option_target`).

use datastar::prelude::PatchElements;
use maud::{html, Markup, PreEscaped};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use pulldown_cmark::{html::push_html, Event, Options, Parser};
use tidepool_harness::ui::{BadgeKind, Ui};

/// The open-sum escape path (B2 law, see `tidepool_harness::ui` module doc):
/// every `Choice` renders this labeled multiline input IN ADDITION to its
/// options, unconditionally. Centralizing it here means a `Choice` render
/// site cannot forget to append it — there is only one call site, below.
fn prose_escape(answer_url: &str) -> Markup {
    html! {
        form class="ui-choice-escape" data-on-submit=(format!("@post('{answer_url}')")) {
            label for="ui-prose-escape-input" { "Or answer in your own words:" }
            textarea
                id="ui-prose-escape-input"
                name="prose"
                data-bind="prose"
                rows="3" {}
            button type="submit" { "Submit" }
        }
    }
}

/// Render `source` as markdown, with raw HTML NEUTRALIZED at the event-stream
/// level: `Event::Html`/`Event::InlineHtml` (pulldown-cmark's "pass this
/// through verbatim" events) are remapped to `Event::Text` before reaching
/// `push_html`, so `push_html`'s own escaping applies to them exactly like
/// any other text run. Real markdown formatting (bold, lists, code, links)
/// is unaffected — only literal HTML tags in the source are neutralized.
fn render_markdown(source: &str) -> Markup {
    let parser = Parser::new_ext(source, Options::empty()).map(|event| match event {
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        other => other,
    });
    let mut rendered = String::new();
    push_html(&mut rendered, parser);
    PreEscaped(rendered)
}

/// Build the `@post('...')` target for one `Choice` option: `answer_url`
/// (harness-controlled, never model text) followed by `/` and the
/// percent-encoded option `key` (model-supplied, untrusted — see the module
/// trust-model doc). Encodes against `NON_ALPHANUMERIC` rather than a
/// hand-rolled unsafe-character list: the key is untrusted input, so
/// correctness beats minimal encoding — this forecloses the `'`
/// JS-string-literal breakout, `/ ? # %` path-semantics confusion, and any
/// control/non-ASCII mischief in one move, with no risk of an omitted
/// character. Axum auto-percent-decodes path params, so the
/// `/answer/{node}/{key}` route handler receives `key` decoded back to its
/// original text; only the JS-string-literal context this fragment is
/// embedded in needs the encoding.
fn choice_option_target(answer_url: &str, key: &str) -> String {
    let encoded_key = utf8_percent_encode(key, NON_ALPHANUMERIC);
    format!("@post('{answer_url}/{encoded_key}')")
}

fn badge_kind_class(kind: BadgeKind) -> &'static str {
    match kind {
        BadgeKind::EffectRow => "effect-row",
        BadgeKind::Fan => "fan",
        BadgeKind::Price => "price",
        BadgeKind::State => "state",
    }
}

/// Table-driven render, one arm per `Ui` constructor. Stateless: this is a
/// pure function of `ui` and `answer_url`.
///
/// KEYED vs UNKEYED: a `TextIn`/`Choice` with a `key` is a FIELD of a form —
/// several keyed widgets in a `Card` render as ONE `<form>` with a single
/// submit, each value bound under `values.<key>` (the typed-form surface,
/// `Tidepool.Form`). Without a key they are standalone: a lone text box, or
/// immediate-post option buttons (the raw `dialogAsk` surface, unchanged).
pub fn render_with_answer_url(ui: &Ui, answer_url: &str) -> Markup {
    match ui {
        // A card containing keyed fields is ONE form with a single submit.
        Ui::Card { title, body } if body.iter().any(has_form_fields) => {
            render_form(title, body, answer_url)
        }
        Ui::Card { title, body } => html! {
            div class="ui-card" {
                @if !title.is_empty() { h3 class="ui-card-title" { (title) } }
                div class="ui-card-body" {
                    @for child in body {
                        (render_with_answer_url(child, answer_url))
                    }
                }
            }
        },
        Ui::Prose { text } => html! {
            div class="ui-prose" { (render_markdown(text)) }
        },
        Ui::Code { lang, source } => html! {
            pre class="ui-code" data-lang=(lang) {
                code { (source) }
            }
        },
        // A keyed widget standalone (no enclosing card): wrap it in its own
        // single-field form so it stays answerable.
        Ui::TextIn { key: Some(_), .. } | Ui::Choice { key: Some(_), .. } => {
            render_form("", std::slice::from_ref(ui), answer_url)
        }
        Ui::Choice {
            prompt,
            options,
            key: None,
        } => html! {
            div class="ui-choice" {
                p class="ui-choice-prompt" { (prompt) }
                div class="ui-choice-options" {
                    @for (key, label) in options {
                        button
                            type="button"
                            class="ui-choice-option"
                            data-on-click=(choice_option_target(answer_url, key)) {
                            (label)
                        }
                    }
                }
                (prose_escape(answer_url))
            }
        },
        Ui::TextIn {
            prompt,
            multiline,
            key: None,
        } => html! {
            form class="ui-textin" data-on-submit=(format!("@post('{answer_url}')")) {
                label for="ui-textin-input" { (prompt) }
                @if *multiline {
                    textarea id="ui-textin-input" name="value" data-bind="value" rows="4" {}
                } @else {
                    input id="ui-textin-input" type="text" name="value" data-bind="value";
                }
                button type="submit" { "Submit" }
            }
        },
        Ui::Badge { label, kind } => html! {
            span class=(format!("ui-badge ui-badge-{}", badge_kind_class(*kind))) { (label) }
        },
    }
}

/// True if `ui` (or a nested card's body) contains a keyed form field.
fn has_form_fields(ui: &Ui) -> bool {
    match ui {
        Ui::TextIn { key: Some(_), .. } | Ui::Choice { key: Some(_), .. } => true,
        Ui::Card { body, .. } => body.iter().any(has_form_fields),
        _ => false,
    }
}

/// Render a card of keyed fields as ONE `<form>` with a single submit — the
/// multi-field typed-form surface. Each keyed field binds under `values.<key>`;
/// the client collects them into `{values:{…}}` on submit.
fn render_form(title: &str, body: &[Ui], answer_url: &str) -> Markup {
    html! {
        form class="ui-card ui-form" data-on-submit=(format!("@post('{answer_url}')")) {
            @if !title.is_empty() { h3 class="ui-card-title" { (title) } }
            div class="ui-card-body" {
                @for child in body { (render_form_field(child)) }
            }
            button type="submit" { "Submit" }
        }
    }
}

/// One member of a form: a keyed text input or radio group, a nested card
/// inline, or a display-only widget. No per-field form/submit — the enclosing
/// form owns the single submit.
fn render_form_field(ui: &Ui) -> Markup {
    match ui {
        Ui::TextIn {
            prompt,
            multiline,
            key: Some(k),
        } => html! {
            div class="ui-field" {
                label class="ui-field-label" { (prompt) }
                @if *multiline {
                    textarea class="ui-field-input" data-bind=(format!("values.{k}")) rows="3" {}
                } @else {
                    input class="ui-field-input" type="text" data-bind=(format!("values.{k}"));
                }
            }
        },
        Ui::Choice {
            prompt,
            options,
            key: Some(k),
        } => html! {
            fieldset class="ui-field ui-radio" {
                legend class="ui-field-label" { (prompt) }
                @for (optk, label) in options {
                    label class="ui-radio-opt" {
                        input type="radio" name=(k) value=(optk) data-bind=(format!("values.{k}"));
                        " " (label)
                    }
                }
            }
        },
        Ui::Card { title, body } => html! {
            div class="ui-subcard" {
                @if !title.is_empty() { div class="ui-subcard-title" { (title) } }
                @for child in body { (render_form_field(child)) }
            }
        },
        Ui::Prose { text } => html! { div class="ui-prose" { (render_markdown(text)) } },
        Ui::Code { lang, source } => html! {
            pre class="ui-code" data-lang=(lang) { code { (source) } }
        },
        Ui::Badge { label, kind } => html! {
            span class=(format!("ui-badge ui-badge-{}", badge_kind_class(*kind))) { (label) }
        },
        // Unkeyed interactive widgets inside a form aren't produced by
        // `Tidepool.Form`; render just the label (no submitting input).
        Ui::TextIn { prompt, key: None, .. } | Ui::Choice { prompt, key: None, .. } => html! {
            div class="ui-field" { label class="ui-field-label" { (prompt) } }
        },
    }
}

/// Wraps `render_with_answer_url`'s markup as a Datastar `datastar-patch-elements`
/// SSE event (the `event:`/`data:` wire text a caller writes straight to an
/// SSE stream). No transport lives here — that's segment 30 C4's job.
pub fn fragment(ui: &Ui, answer_url: &str) -> String {
    let markup = render_with_answer_url(ui, answer_url).into_string();
    PatchElements::new(markup).into_datastar_event().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "/answer/n1";

    #[test]
    fn snapshot_prose() {
        let ui = Ui::Prose {
            text: "hello **world**\n\n```rust\nfn f() {}\n```".into(),
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert_eq!(
            rendered,
            "<div class=\"ui-prose\"><p>hello <strong>world</strong></p>\n\
             <pre><code class=\"language-rust\">fn f() {}\n\
             </code></pre>\n\
             </div>"
        );
    }

    #[test]
    fn snapshot_code() {
        let ui = Ui::Code {
            lang: "haskell".into(),
            source: "resume :: Verdict -> M ()".into(),
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert_eq!(
            rendered,
            "<pre class=\"ui-code\" data-lang=\"haskell\">\
             <code>resume :: Verdict -&gt; M ()</code></pre>"
        );
    }

    #[test]
    fn snapshot_choice() {
        let ui = Ui::Choice {
            prompt: "verdict?".into(),
            options: vec![
                ("approve".into(), "Approve".into()),
                ("reject".into(), "Reject".into()),
            ],
            key: None,
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert_eq!(
            rendered,
            "<div class=\"ui-choice\">\
             <p class=\"ui-choice-prompt\">verdict?</p>\
             <div class=\"ui-choice-options\">\
             <button type=\"button\" class=\"ui-choice-option\" data-on-click=\"@post('/answer/n1/approve')\">Approve</button>\
             <button type=\"button\" class=\"ui-choice-option\" data-on-click=\"@post('/answer/n1/reject')\">Reject</button>\
             </div>\
             <form class=\"ui-choice-escape\" data-on-submit=\"@post('/answer/n1')\">\
             <label for=\"ui-prose-escape-input\">Or answer in your own words:</label>\
             <textarea id=\"ui-prose-escape-input\" name=\"prose\" data-bind=\"prose\" rows=\"3\"></textarea>\
             <button type=\"submit\">Submit</button>\
             </form>\
             </div>"
        );
    }

    /// SECURITY: a `Prose` body containing raw `<script>`/`<img onerror>`
    /// markup must render as escaped literal text, never live DOM — the eDSL
    /// must be structurally unable to emit HTML the browser executes.
    #[test]
    fn prose_html_is_escaped_not_live() {
        let ui = Ui::Prose {
            text: "<script>alert(1)</script><img src=x onerror=alert(1)>".into(),
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert!(
            rendered.contains("&lt;script&gt;"),
            "script tag must be escaped: {rendered}"
        );
        assert!(
            !rendered.contains("<script>"),
            "must not contain a live <script> tag: {rendered}"
        );
        assert!(
            !rendered.contains("<img"),
            "must not contain a live <img> tag (onerror= as escaped text is fine, \
             onerror= as a real attribute on a live element is not): {rendered}"
        );
    }

    /// SECURITY: a model-supplied `Choice` option key containing a single
    /// quote must not break out of the `@post('...')` JS string literal it's
    /// interpolated into.
    #[test]
    fn choice_key_with_quote_cannot_break_out_of_post_literal() {
        let ui = Ui::Choice {
            prompt: "p?".into(),
            options: vec![("x')//".into(), "Evil".into())],
            key: None,
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        // The raw key must never appear unescaped inside the single-quoted target.
        assert!(
            !rendered.contains("@post('/answer/n1/x')//')"),
            "unescaped key broke out of the @post('...') literal: {rendered}"
        );
        assert!(
            rendered.contains("data-on-click=\"@post('/answer/n1/x%27%29%2F%2F')\""),
            "expected the percent-encoded key in the post target: {rendered}"
        );
    }

    #[test]
    fn snapshot_textin_singleline() {
        let ui = Ui::TextIn {
            prompt: "name?".into(),
            multiline: false,
            key: None,
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert_eq!(
            rendered,
            "<form class=\"ui-textin\" data-on-submit=\"@post('/answer/n1')\">\
             <label for=\"ui-textin-input\">name?</label>\
             <input id=\"ui-textin-input\" type=\"text\" name=\"value\" data-bind=\"value\">\
             <button type=\"submit\">Submit</button>\
             </form>"
        );
    }

    #[test]
    fn snapshot_textin_multiline() {
        let ui = Ui::TextIn {
            prompt: "notes?".into(),
            multiline: true,
            key: None,
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert_eq!(
            rendered,
            "<form class=\"ui-textin\" data-on-submit=\"@post('/answer/n1')\">\
             <label for=\"ui-textin-input\">notes?</label>\
             <textarea id=\"ui-textin-input\" name=\"value\" data-bind=\"value\" rows=\"4\"></textarea>\
             <button type=\"submit\">Submit</button>\
             </form>"
        );
    }

    /// A card of KEYED fields renders as ONE form with a single submit, each
    /// value bound under `values.<key>`; a keyed Choice becomes a radio group
    /// (not immediate-post buttons). This is the multi-field typed-form surface.
    #[test]
    fn keyed_card_renders_one_form_with_bound_fields() {
        let ui = Ui::Card {
            title: "Reply".into(),
            body: vec![
                Ui::TextIn {
                    prompt: "Notes".into(),
                    multiline: false,
                    key: Some("f0".into()),
                },
                Ui::Choice {
                    prompt: "Lane".into(),
                    options: vec![("a".into(), "Alpha".into()), ("b".into(), "Beta".into())],
                    key: Some("f1".into()),
                },
            ],
        };
        let r = render_with_answer_url(&ui, URL).into_string();
        assert_eq!(r.matches("<form").count(), 1, "exactly one form: {r}");
        assert!(r.contains("data-on-submit=\"@post('/answer/n1')\""));
        assert!(r.contains("data-bind=\"values.f0\""), "text field bound: {r}");
        // keyed Choice → radios grouped by the field key, bound under values.f1
        assert!(r.contains("type=\"radio\""), "choice is a radio: {r}");
        assert!(r.contains("name=\"f1\""));
        assert!(r.contains("value=\"a\""));
        assert!(r.contains("data-bind=\"values.f1\""));
        assert!(r.contains("<button type=\"submit\">Submit</button>"));
        // NOT the standalone immediate-post buttons:
        assert!(!r.contains("ui-choice-option"), "no immediate-post buttons: {r}");
    }

    #[test]
    fn snapshot_badge() {
        let ui = Ui::Badge {
            label: "Exec, Fs".into(),
            kind: BadgeKind::EffectRow,
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert_eq!(
            rendered,
            "<span class=\"ui-badge ui-badge-effect-row\">Exec, Fs</span>"
        );
    }

    #[test]
    fn snapshot_card_nested() {
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
                },
                Ui::Badge {
                    label: "Exec, Fs".into(),
                    kind: BadgeKind::EffectRow,
                },
            ],
        };
        let rendered = render_with_answer_url(&ui, URL).into_string();
        assert!(
            rendered.starts_with("<div class=\"ui-card\"><h3 class=\"ui-card-title\">hole</h3>")
        );
        assert!(rendered.contains("<pre class=\"ui-code\" data-lang=\"haskell\">"));
        assert!(rendered.contains("class=\"ui-choice\""));
        assert!(rendered.contains("class=\"ui-choice-escape\""));
        assert!(rendered.contains("<span class=\"ui-badge ui-badge-effect-row\">Exec, Fs</span>"));
        assert!(rendered.ends_with("</div>"));
    }

    #[test]
    fn snapshot_fragment_envelope() {
        let ui = Ui::Prose { text: "hi".into() };
        let event = fragment(&ui, URL);
        assert_eq!(
            event,
            "event: datastar-patch-elements\n\
             data: elements <div class=\"ui-prose\"><p>hi</p>\n\
             data: elements </div>\n\n"
        );
    }

    /// B2 property: every rendered `Choice`, anywhere in the tree, carries
    /// the prose-escape marker — it cannot be omitted by a call site.
    #[test]
    fn every_choice_has_prose_escape() {
        let trees = [
            Ui::Choice {
                prompt: "a?".into(),
                options: vec![],
                key: None,
            },
            Ui::Choice {
                prompt: "b?".into(),
                options: vec![("x".into(), "X".into()), ("y".into(), "Y".into())],
                key: None,
            },
            Ui::Card {
                title: "outer".into(),
                body: vec![
                    Ui::Prose {
                        text: "intro".into(),
                    },
                    Ui::Card {
                        title: "inner".into(),
                        body: vec![Ui::Choice {
                            prompt: "c?".into(),
                            options: vec![],
                            key: None,
                        }],
                    },
                    Ui::Choice {
                        prompt: "d?".into(),
                        options: vec![("k".into(), "K".into())],
                        key: None,
                    },
                ],
            },
        ];

        for ui in &trees {
            let rendered = render_with_answer_url(ui, URL).into_string();
            let choices = rendered.matches("class=\"ui-choice\"").count();
            let escapes = rendered.matches("class=\"ui-choice-escape\"").count();
            assert!(choices > 0, "test fixture has no Choice: {rendered}");
            assert_eq!(
                escapes, choices,
                "expected one prose escape per Choice, got {escapes} escapes for {choices} choices: {rendered}"
            );
        }
    }
}
