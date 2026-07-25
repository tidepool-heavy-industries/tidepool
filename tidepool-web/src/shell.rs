//! The observatory pane shell — a fixed 3-pane layout (tree · inspector/form ·
//! log tail) rendered server-side with maud, patched over SSE.
//!
//! # CSS choice (ruling D5 — built RIGHT)
//!
//! A hand-authored, VENDORED design system: CSS custom properties (design
//! tokens) + CSS Grid for the fixed layout, inlined in the served page. No
//! framework, no CDN, no build step — a Tailwind/Bootstrap would be either
//! CDN-shaped or a build step, both forbidden. ~1 screen of deliberate tokens
//! (a dark palette, a type scale, spacing units) drives a real fixed layout:
//! a left tree rail, a center inspector, a right log tail, each independently
//! scrollable, header pinned. This is a coherent design system, not placeholder
//! styling — the point of D5 is that jank is more expensive to debug later than
//! to avoid now.
//!
//! # Client runtime (vendored, no CDN)
//!
//! The renderer (`crate::render`) emits real Datastar `datastar-patch-elements`
//! SSE frames and `data-on-click`/`data-on-submit="@post(...)"` attributes. The
//! browser side is a small VENDORED vanilla-JS client (`OBSERVATORY_JS`) that
//! speaks exactly that wire: it opens the SSE stream, applies patch-elements
//! frames by replacing the target element by id, and wires the `data-on-*`
//! attributes to `fetch` POSTs. Self-contained, no third-party blob to verify,
//! honors the same contract the renderer already emits.

use maud::{html, Markup, PreEscaped, DOCTYPE};

/// The vendored design-system stylesheet. Dark palette, fixed 3-column grid,
/// per-pane scroll, pinned header.
pub const OBSERVATORY_CSS: &str = r#"
:root {
  --bg: #0f1117; --bg-raised: #161923; --bg-inset: #1d2130;
  --border: #262b3a; --border-strong: #333a4f;
  --fg: #e6e9f0; --fg-dim: #9aa3b8; --fg-faint: #6b7488;
  --accent: #6aa3ff; --accent-dim: #3a5a99;
  --ok: #5ad19a; --warn: #e6b455; --err: #ff6b6b; --fork: #c58af9;
  --mono: ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace;
  --sans: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --s1: 4px; --s2: 8px; --s3: 12px; --s4: 16px; --s5: 24px;
  --r: 6px;
}
* { box-sizing: border-box; }
html, body { height: 100%; margin: 0; }
body {
  background: var(--bg); color: var(--fg); font-family: var(--sans);
  font-size: 14px; line-height: 1.5; overflow: hidden;
}
.app {
  display: grid; height: 100vh;
  grid-template-rows: auto 1fr;
  grid-template-columns: minmax(220px, 280px) 1fr minmax(260px, 320px) minmax(280px, 380px);
  grid-template-areas: "hdr hdr hdr hdr" "tree main side log";
}
header.hdr {
  grid-area: hdr; display: flex; align-items: center; gap: var(--s4);
  padding: var(--s3) var(--s4); background: var(--bg-raised);
  border-bottom: 1px solid var(--border);
}
header.hdr .brand { font-weight: 600; letter-spacing: .3px; }
header.hdr .brand .glyph { color: var(--accent); }
header.hdr .status { margin-left: auto; font-size: 12px; color: var(--fg-dim); }
header.hdr .status.signed-in { color: var(--ok); }
header.hdr .status.signed-out { color: var(--warn); }

.pane { overflow-y: auto; padding: var(--s3); }
.pane.tree { grid-area: tree; background: var(--bg-raised); border-right: 1px solid var(--border); }
.pane.main { grid-area: main; }
.pane.side { grid-area: side; background: var(--bg-raised); border-left: 1px solid var(--border); border-right: 1px solid var(--border); display: flex; flex-direction: column; padding: 0; overflow: hidden; }
.pane.side .sub { flex: 1 1 0; min-height: 0; overflow-y: auto; padding: var(--s3); }
.pane.side .sub + .sub { border-top: 1px solid var(--border); }
.pane.log  { grid-area: log; background: var(--bg-inset); border-left: 1px solid var(--border); font-family: var(--mono); font-size: 12px; }
.pane h2 { font-size: 11px; text-transform: uppercase; letter-spacing: .8px; color: var(--fg-faint); margin: 0 0 var(--s3); font-weight: 600; }

/* tree */
.node { padding: var(--s2) var(--s3); border-radius: var(--r); margin-bottom: var(--s1); border: 1px solid transparent; cursor: default; }
.node:hover { background: var(--bg-inset); }
.node.suspended { border-color: var(--accent-dim); }
.node .teaser { font-weight: 500; }
.node .meta { display: flex; gap: var(--s2); margin-top: var(--s1); flex-wrap: wrap; }
.node.child { margin-left: var(--s4); }
.chip { font-size: 10px; padding: 1px var(--s2); border-radius: 999px; background: var(--bg); border: 1px solid var(--border-strong); color: var(--fg-dim); }
.chip.state-running { color: var(--accent); border-color: var(--accent-dim); }
.chip.state-suspended { color: var(--warn); border-color: var(--warn); }
.chip.state-done { color: var(--ok); border-color: var(--ok); }
.chip.state-thunk { color: var(--fg-faint); }
.chip.state-cancelled { color: var(--err); border-color: var(--err); }
.chip.fork { color: var(--fork); border-color: var(--fork); }
.chip.escalated { color: var(--err); border-color: var(--err); }
.node .actions { margin-top: var(--s2); }
button {
  font: inherit; font-size: 12px; padding: var(--s1) var(--s3);
  background: var(--accent-dim); color: var(--fg); border: 1px solid var(--accent);
  border-radius: var(--r); cursor: pointer;
}
button:hover { background: var(--accent); }
button.ghost { background: transparent; border-color: var(--border-strong); color: var(--fg-dim); }
button.ghost:hover { border-color: var(--fg-dim); color: var(--fg); }

/* inspector / form */
.ui-card { background: var(--bg-raised); border: 1px solid var(--border); border-radius: var(--r); padding: var(--s4); margin-bottom: var(--s4); }
.ui-card-title { margin: 0 0 var(--s3); font-size: 15px; }
.ui-prose { margin-bottom: var(--s3); }
.ui-prose p { margin: 0 0 var(--s2); }
.ui-code, pre { background: var(--bg-inset); border: 1px solid var(--border); border-radius: var(--r); padding: var(--s3); overflow-x: auto; font-family: var(--mono); font-size: 12px; }
.ui-choice-prompt { font-weight: 500; margin: 0 0 var(--s2); }
.ui-choice-options { display: flex; gap: var(--s2); flex-wrap: wrap; margin-bottom: var(--s3); }
.ui-choice-option { background: var(--bg-inset); border: 1px solid var(--border-strong); color: var(--fg); }
.ui-choice-option:hover { border-color: var(--accent); }
.ui-choice-escape, .ui-textin { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s2); }
.ui-choice-escape label, .ui-textin label { font-size: 12px; color: var(--fg-dim); }
textarea, input[type=text] { font: inherit; background: var(--bg-inset); color: var(--fg); border: 1px solid var(--border-strong); border-radius: var(--r); padding: var(--s2); resize: vertical; }
textarea:focus, input:focus { outline: none; border-color: var(--accent); }
.ui-badge { display: inline-block; font-size: 10px; padding: 1px var(--s2); border-radius: 999px; background: var(--bg); border: 1px solid var(--border-strong); color: var(--fg-dim); margin-right: var(--s1); }
.ui-card.escalation { border-color: var(--err); }
.escalation-reason { color: var(--warn); font-size: 12px; margin: 0 0 var(--s3); }
.escalation-allocate { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s3); }
.escalation-allocate label { font-size: 12px; color: var(--fg-dim); }
.escalation-abort { margin-top: var(--s3); }
.empty { color: var(--fg-faint); font-style: italic; padding: var(--s4); }

/* meters */
.meter-rollup { font-size: 12px; color: var(--fg-dim); margin-bottom: var(--s3); }
.meter-rollup b { color: var(--fg); font-weight: 600; }
table.meter-table { width: 100%; border-collapse: collapse; font-size: 12px; }
table.meter-table th, table.meter-table td { text-align: left; padding: 2px var(--s2); border-bottom: 1px solid var(--border); }
table.meter-table th { color: var(--fg-faint); font-weight: 500; }

/* heap */
table.heap-table { width: 100%; border-collapse: collapse; font-size: 12px; }
table.heap-table th, table.heap-table td { text-align: left; padding: 2px var(--s2); border-bottom: 1px solid var(--border); }
table.heap-table th { color: var(--fg-faint); font-weight: 500; }

/* trace */
.trace-node { margin-bottom: var(--s2); }
.trace-node summary { cursor: pointer; font-size: 12px; color: var(--fg-dim); padding: var(--s1) 0; }
.trace-node summary:hover { color: var(--fg); }
.trace-row { padding: var(--s2) 0; border-top: 1px dashed var(--border); font-family: var(--mono); font-size: 11px; }
.trace-row .chip { margin-bottom: var(--s1); }
.trace-req, .trace-resp { white-space: pre-wrap; word-break: break-word; color: var(--fg-dim); margin-top: 2px; }

/* log */
.log-line { padding: 2px 0; border-bottom: 1px solid var(--border); white-space: pre-wrap; word-break: break-word; }
.log-line .ev { color: var(--accent); }
.log-line .node-id { color: var(--fg-faint); }
"#;

/// The vendored client runtime. Implements the exact Datastar subset the
/// renderer emits: SSE `datastar-patch-elements` frames (replace by id) and
/// `data-on-click`/`data-on-submit="@post(url)"` handlers.
pub const OBSERVATORY_JS: &str = r#"
(function () {
  // Apply one datastar-patch-elements payload: the `data: elements <html>`
  // lines carry an element; we replace the same-id element in the DOM (or
  // append into a named region for regions like #tree / #log / #inspector).
  function applyPatch(html) {
    const tpl = document.createElement('template');
    tpl.innerHTML = html.trim();
    const frag = tpl.content;
    frag.querySelectorAll('[id]').forEach((el) => {
      const existing = document.getElementById(el.id);
      if (existing) existing.replaceWith(el);
      else document.body.appendChild(el);
    });
    wire(document);
  }

  // Wire data-on-* handlers within a root.
  function wire(root) {
    root.querySelectorAll('[data-on-click]').forEach((el) => {
      if (el.__wired) return; el.__wired = true;
      el.addEventListener('click', () => post(parsePost(el.getAttribute('data-on-click')), el));
    });
    root.querySelectorAll('[data-on-submit]').forEach((form) => {
      if (form.__wired) return; form.__wired = true;
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const body = {};
        form.querySelectorAll('[data-bind]').forEach((f) => { body[f.getAttribute('data-bind')] = f.value; });
        post(parsePost(form.getAttribute('data-on-submit')), form, body);
      });
    });
  }

  // Extract the URL from a `@post('/x')` expression.
  function parsePost(expr) {
    const m = /@post\(\s*'([^']*)'\s*\)/.exec(expr || '');
    return m ? m[1] : null;
  }

  function post(url, el, body) {
    if (!url) return;
    fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body || {}),
    }).catch((e) => console.error('post failed', url, e));
  }

  // SSE stream of log-driven fragments.
  function connect() {
    const es = new EventSource('/sse');
    es.addEventListener('datastar-patch-elements', (ev) => {
      // Datastar frames put element HTML on `elements ` prefixed data lines;
      // the SDK joins them with newlines, so strip the prefix per line.
      const html = ev.data.split('\n')
        .map((l) => l.replace(/^elements /, ''))
        .join('\n');
      applyPatch(html);
    });
    es.onerror = () => { /* browser auto-reconnects */ };
  }

  window.addEventListener('DOMContentLoaded', () => { wire(document); connect(); });
})();
"#;

/// Render the full observatory page shell. `signed_in` drives the auth banner;
/// `tree` / `inspector` / `meters` / `trace` / `heap` / `log` are the initial
/// server-rendered pane contents (later patched over SSE).
#[allow(clippy::too_many_arguments)]
pub fn page(
    signed_in: bool,
    tree: Markup,
    inspector: Markup,
    meters: Markup,
    trace: Markup,
    heap: Markup,
    log: Markup,
) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "tidepool observatory" }
                style { (PreEscaped(OBSERVATORY_CSS)) }
                script { (PreEscaped(OBSERVATORY_JS)) }
            }
            body {
                div class="app" {
                    header class="hdr" {
                        span class="brand" { span class="glyph" { "◊ " } "tidepool observatory" }
                        @if signed_in {
                            span class="status signed-in" { "● signed in" }
                        } @else {
                            span class="status signed-out" {
                                "○ signed out — "
                                button class="ghost" data-on-click="@post('/auth/start')" { "sign in" }
                            }
                        }
                    }
                    div class="pane tree" {
                        h2 { "cognition tree" }
                        div id="tree" { (tree) }
                    }
                    div class="pane main" {
                        h2 { "inspector" }
                        div id="inspector" { (inspector) }
                    }
                    div class="pane side" {
                        div class="sub" {
                            h2 { "meters" }
                            div id="meters" { (meters) }
                        }
                        div class="sub" {
                            h2 { "trace" }
                            div id="trace" { (trace) }
                        }
                        div class="sub" {
                            h2 { "heap" }
                            div id="heap" { (heap) }
                        }
                    }
                    div class="pane log" {
                        h2 { "event log" }
                        div id="log" { (log) }
                    }
                }
            }
        }
    }
}

/// The empty-inspector placeholder.
pub fn inspector_empty() -> Markup {
    html! { div class="empty" { "No hole selected. Force a node or answer a pending hole." } }
}
