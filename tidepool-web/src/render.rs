//! `Ui` → Datastar-ready HTML fragment renderer.
//!
//! Every function here is a pure function of a `&Ui` value (plus an
//! `answer_url` base — the harness's answer verbs don't exist yet, segment
//! 30 C4 wires the real endpoints in, so callers pass whatever base they
//! have). No state lives here: nothing is cached, nothing is mutated,
//! nothing is read back out of the `Ui` tree between renders.
//!
//! Markdown trust model: `Prose` markdown is rendered as HTML, including
//! fenced code blocks, without sanitization. `Ui` values are always
//! operator-authored (constructed by the harness / its Haskell programs,
//! never by an untrusted third party) and tidepool-web binds loopback-only
//! (see `tidepool-web/CLAUDE.md`), so there is no cross-tenant HTML
//! injection surface to defend against here.

use datastar::prelude::PatchElements;
use maud::{html, Markup, PreEscaped};
use pulldown_cmark::{html::push_html, Options, Parser};
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

fn render_markdown(source: &str) -> Markup {
    let parser = Parser::new_ext(source, Options::empty());
    let mut rendered = String::new();
    push_html(&mut rendered, parser);
    PreEscaped(rendered)
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
pub fn render_with_answer_url(ui: &Ui, answer_url: &str) -> Markup {
    match ui {
        Ui::Card { title, body } => html! {
            div class="ui-card" {
                h3 class="ui-card-title" { (title) }
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
        Ui::Choice { prompt, options } => html! {
            div class="ui-choice" {
                p class="ui-choice-prompt" { (prompt) }
                div class="ui-choice-options" {
                    @for (key, label) in options {
                        button
                            type="button"
                            class="ui-choice-option"
                            data-on-click=(format!("@post('{answer_url}/{key}')")) {
                            (label)
                        }
                    }
                }
                (prose_escape(answer_url))
            }
        },
        Ui::TextIn { prompt, multiline } => html! {
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

    #[test]
    fn snapshot_textin_singleline() {
        let ui = Ui::TextIn {
            prompt: "name?".into(),
            multiline: false,
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
            },
            Ui::Choice {
                prompt: "b?".into(),
                options: vec![("x".into(), "X".into()), ("y".into(), "Y".into())],
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
                        }],
                    },
                    Ui::Choice {
                        prompt: "d?".into(),
                        options: vec![("k".into(), "K".into())],
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
