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
.pane.main { grid-area: main; display: flex; flex-direction: column; padding: 0; overflow: hidden; }
.pane.main .sub-inspector { flex: 0 0 auto; max-height: 42%; overflow-y: auto; padding: var(--s3); border-bottom: 1px solid var(--border); }
.pane.main .sub-transcript { flex: 1 1 0; min-height: 0; overflow-y: auto; padding: var(--s3); }
.pane.side { grid-area: side; background: var(--bg-raised); border-left: 1px solid var(--border); border-right: 1px solid var(--border); display: flex; flex-direction: column; padding: 0; overflow: hidden; }
.pane.side .sub { flex: 1 1 0; min-height: 0; overflow-y: auto; padding: var(--s3); }
.pane.side .sub + .sub { border-top: 1px solid var(--border); }
.pane.side details.sub-collapse { flex: 0 0 auto; overflow-y: auto; padding: 0; }
.pane.side details.sub-collapse > summary { cursor: pointer; font-size: 11px; text-transform: uppercase; letter-spacing: .8px; color: var(--fg-faint); font-weight: 600; padding: var(--s3) var(--s3) var(--s2); list-style: none; }
.pane.side details.sub-collapse > summary::-webkit-details-marker { display: none; }
.pane.side details.sub-collapse > summary::before { content: "▸ "; }
.pane.side details.sub-collapse[open] > summary::before { content: "▾ "; }
.pane.side details.sub-collapse > div { padding: 0 var(--s3) var(--s3); }
.pane.log  { grid-area: log; background: var(--bg-inset); border-left: 1px solid var(--border); font-family: var(--mono); font-size: 12px; }
.pane h2 { font-size: 11px; text-transform: uppercase; letter-spacing: .8px; color: var(--fg-faint); margin: 0 0 var(--s3); font-weight: 600; }

/* tree */
.node { padding: var(--s2) var(--s3); border-radius: var(--r); margin-bottom: var(--s1); border: 1px solid transparent; cursor: pointer; }
.node:hover { background: var(--bg-inset); border-color: var(--border-strong); }
.node.selected { background: var(--bg-inset); border-color: var(--accent); }
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

/* multi-field typed form (Tidepool.Form) */
.ui-form { display: flex; flex-direction: column; gap: var(--s3); }
.ui-field { display: flex; flex-direction: column; gap: var(--s1); border: 0; margin: 0; padding: 0; }
.ui-field-label { font-size: 12px; color: var(--fg-dim); font-weight: 500; }
.ui-field-input { width: 100%; }
.ui-radio { border: 1px solid var(--border); border-radius: var(--r); padding: var(--s2) var(--s3); }
.ui-radio-opt { display: flex; align-items: center; gap: var(--s2); font-size: 13px; color: var(--fg); padding: 1px 0; cursor: pointer; }
.ui-radio-opt input[type=radio] { accent-color: var(--accent); }
.ui-subcard { border-left: 2px solid var(--border-strong); padding-left: var(--s3); display: flex; flex-direction: column; gap: var(--s3); }
.ui-subcard-title { font-size: 12px; text-transform: uppercase; letter-spacing: .5px; color: var(--fg-faint); }
.ui-form > button[type=submit] { align-self: flex-start; }
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

/* transcript */
.tx-focus { font-size: 12px; color: var(--accent); background: var(--bg-inset); border: 1px solid var(--accent-dim); border-radius: var(--r); padding: var(--s2) var(--s3); margin-bottom: var(--s3); }
.tx-node { margin-bottom: var(--s5); }
.tx-node-hdr { font-size: 11px; text-transform: uppercase; letter-spacing: .6px; color: var(--fg-faint); margin-bottom: var(--s2); }
.tx-turn { margin-bottom: var(--s3); border-left: 2px solid var(--border-strong); padding-left: var(--s3); }
.tx-turn > summary { cursor: pointer; font-size: 12px; color: var(--fg-dim); list-style: none; }
.tx-turn > summary::-webkit-details-marker { display: none; }
.tx-turn > summary::before { content: "▾ "; color: var(--fg-faint); }
.tx-turn:not([open]) > summary::before { content: "▸ "; }
.tx-assistant { border-left-color: var(--accent-dim); }
.tx-user { border-left-color: var(--fg-faint); }
.tx-system { border-left-color: var(--border-strong); }
.tx-role { font-weight: 600; color: var(--fg); }
.tx-toks { color: var(--fg-faint); font-size: 11px; }
.tx-body { margin: var(--s2) 0 0; max-height: 360px; overflow: auto; white-space: pre-wrap; word-break: break-word; }
.tx-result { margin: var(--s2) 0 var(--s3); padding-left: var(--s3); border-left: 2px solid var(--ok); }
.tx-result .tx-arrow { color: var(--ok); font-size: 12px; font-weight: 600; }
.tx-hole { margin: var(--s2) 0 var(--s3); padding-left: var(--s3); border-left: 2px solid var(--warn); }
.tx-hole .tx-arrow { color: var(--warn); font-size: 12px; font-weight: 600; }
.tx-hole-prompt { color: var(--fg-dim); font-size: 12px; margin-top: 2px; }
.tx-type { font-family: var(--mono); color: var(--accent); font-size: 12px; }
.tx-error { margin: var(--s2) 0 var(--s3); padding-left: var(--s3); border-left: 2px solid var(--err); color: var(--err); font-size: 12px; }
.tx-error .tx-arrow { font-weight: 600; }

/* thinking (reasoning summary) */
.tx-think { margin: var(--s1) 0 var(--s2); background: var(--bg-inset); border: 1px solid var(--border); border-radius: var(--r); }
.tx-think > summary { cursor: pointer; font-size: 11px; color: var(--fork); padding: var(--s1) var(--s2); list-style: none; }
.tx-think > summary::-webkit-details-marker { display: none; }
.tx-think > summary::before { content: "▸ "; }
.tx-think[open] > summary::before { content: "▾ "; }
.tx-think-body { margin: 0; padding: var(--s2); max-height: 220px; overflow: auto; white-space: pre-wrap; word-break: break-word; color: var(--fg-dim); font-size: 11px; background: transparent; border: none; border-top: 1px solid var(--border); }

/* follow-up composer */
.tx-followup { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s4); padding-top: var(--s3); border-top: 1px solid var(--border); }
.tx-followup textarea { width: 100%; font: inherit; }
.tx-followup button { align-self: flex-start; }

/* live streaming turn */
.tx-live { border-left: 2px solid var(--accent); padding-left: var(--s3); margin-bottom: var(--s3); }
.tx-live-hdr { font-size: 12px; color: var(--fg-dim); }
.tx-streaming { color: var(--accent); font-size: 11px; animation: pulse 1.4s ease-in-out infinite; }
.tx-cursor { color: var(--accent); animation: pulse 1s steps(2) infinite; }
@keyframes pulse { 50% { opacity: .35; } }

/* feedback: toast, in-flight, connection */
#toast {
  position: fixed; bottom: var(--s4); right: var(--s4); max-width: 460px;
  padding: var(--s3) var(--s4); border-radius: var(--r); font-size: 13px;
  box-shadow: 0 6px 24px rgba(0,0,0,.45); z-index: 50; pointer-events: none;
  opacity: 0; transform: translateY(8px); transition: opacity .15s, transform .15s;
  white-space: pre-wrap; word-break: break-word;
}
#toast.show { opacity: 1; transform: translateY(0); }
#toast.err { background: #2a1416; border: 1px solid var(--err); color: #ffd7d7; }
#toast.ok  { background: #12241a; border: 1px solid var(--ok);  color: #cdeadd; }
.pending { opacity: .6; cursor: progress !important; }
button:disabled { opacity: .5; cursor: progress; }
.conn { font-size: 12px; margin-left: var(--s3); }
.conn.ok { color: var(--ok); }
.conn.down { color: var(--warn); }
"#;

/// The vendored client runtime. Implements the exact Datastar subset the
/// renderer emits: SSE `datastar-patch-elements` frames (replace by id) and
/// `data-on-click`/`data-on-submit="@post(url)"` handlers.
pub const OBSERVATORY_JS: &str = r#"
(function () {
  // Apply one datastar-patch-elements payload: each `data: elements <html>`
  // line carries an element; we replace the same-id element in place. Two
  // things are PRESERVED across the replace so live SSE ticks don't clobber
  // what the operator is doing: (1) a pane the operator is focused in / typing
  // in is left untouched this tick, and (2) the open/closed state of every
  // <details id=...> is carried over. Without these, every turn that lands
  // would reset expanded transcript turns and wipe half-typed answers.
  function applyPatch(html) {
    const tpl = document.createElement('template');
    tpl.innerHTML = html.trim();
    tpl.content.querySelectorAll('[id]').forEach((next) => {
      const cur = document.getElementById(next.id);
      if (!cur) { document.body.appendChild(next); wire(next); return; }
      const active = document.activeElement;
      if (active && active !== document.body && cur.contains(active)) return; // don't rip out a focused pane
      const openState = {};
      cur.querySelectorAll('details[id]').forEach((d) => { openState[d.id] = d.open; });
      cur.replaceWith(next);
      next.querySelectorAll('details[id]').forEach((d) => { if (d.id in openState) d.open = openState[d.id]; });
      wire(next);
    });
  }

  // Wire data-on-* handlers within a root (idempotent via __wired).
  function wire(root) {
    root.querySelectorAll('[data-on-click]').forEach((el) => {
      if (el.__wired) return; el.__wired = true;
      // stopPropagation so a button inside a clickable node fires only its own
      // verb, not the node's select.
      el.addEventListener('click', (e) => { e.stopPropagation(); post(parsePost(el.getAttribute('data-on-click')), el, null, null); });
    });
    root.querySelectorAll('[data-on-submit]').forEach((form) => {
      if (form.__wired) return; form.__wired = true;
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const body = {};
        form.querySelectorAll('[data-bind]').forEach((f) => {
          // A radio group shares one data-bind; only the selected one counts.
          if (f.type === 'radio' && !f.checked) return;
          const path = f.getAttribute('data-bind');
          const dot = path.indexOf('.');
          if (dot > 0) {
            // Nested field, e.g. `values.f0` → body.values.f0 (multi-field form).
            const parent = path.slice(0, dot), child = path.slice(dot + 1);
            (body[parent] = body[parent] || {})[child] = f.value;
          } else {
            body[path] = f.value;
          }
        });
        post(parsePost(form.getAttribute('data-on-submit')), form, body, form);
      });
    });
  }

  // Extract the URL from a `@post('/x')` expression.
  function parsePost(expr) {
    const m = /@post\(\s*'([^']*)'\s*\)/.exec(expr || '');
    return m ? m[1] : null;
  }

  // POST a verb, surfacing the outcome: an in-flight `.pending` state + disabled
  // submit button, then a toast on failure (server sends {ok:false,error} with a
  // 4xx; fetch does NOT reject on those, so we must inspect the response). On
  // success a submitted form is cleared.
  async function post(url, el, body, form) {
    if (!url) return;
    const btn = (el && el.tagName === 'BUTTON') ? el
              : (form ? form.querySelector('button[type=submit]') : null);
    if (el) el.classList.add('pending');
    if (btn) btn.disabled = true;
    try {
      const res = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body || {}),
      });
      let data = {};
      try { data = await res.json(); } catch (_) {}
      if (!res.ok || data.ok === false) {
        toast(data.error || (res.status + ' ' + res.statusText), true);
      } else {
        toast('', false);
        if (form) form.querySelectorAll('[data-bind]').forEach((f) => { f.value = ''; });
      }
    } catch (e) {
      toast('network error — is the server up? (' + e.message + ')', true);
    } finally {
      if (el) el.classList.remove('pending');
      if (btn) btn.disabled = false;
    }
  }

  // A single bottom-right toast, reused. Empty msg dismisses it.
  function toast(msg, isErr) {
    let t = document.getElementById('toast');
    if (!t) { t = document.createElement('div'); t.id = 'toast'; document.body.appendChild(t); }
    if (!msg) { t.classList.remove('show'); return; }
    t.className = 'toast show ' + (isErr ? 'err' : 'ok');
    t.textContent = msg;
    clearTimeout(t.__timer);
    t.__timer = setTimeout(() => t.classList.remove('show'), 7000);
  }

  // Live/reconnecting indicator in the header — so a dropped stream (e.g. a
  // server restart) reads as "reconnecting", not a frozen page.
  function setConn(ok) {
    const s = document.getElementById('conn');
    if (!s) return;
    s.className = 'conn ' + (ok ? 'ok' : 'down');
    s.textContent = ok ? '● live' : '○ reconnecting…';
  }

  // SSE stream of log-driven fragments.
  function connect() {
    const es = new EventSource('/sse');
    es.onopen = () => setConn(true);
    es.addEventListener('datastar-patch-elements', (ev) => {
      // Datastar frames put element HTML on `elements ` prefixed data lines;
      // the SDK joins them with newlines, so strip the prefix per line.
      const html = ev.data.split('\n')
        .map((l) => l.replace(/^elements /, ''))
        .join('\n');
      applyPatch(html);
    });
    es.onerror = () => setConn(false); // EventSource auto-reconnects; onopen flips back
  }

  window.addEventListener('DOMContentLoaded', () => { wire(document); connect(); });
})();
"#;

/// Render the full observatory page shell. `signed_in` drives the auth banner;
/// `tree` / `inspector` / `transcript` / `meters` / `trace` / `heap` / `log`
/// are the initial server-rendered pane contents (later patched over SSE).
#[allow(clippy::too_many_arguments)]
pub fn page(
    signed_in: bool,
    tree: Markup,
    inspector: Markup,
    meters: Markup,
    trace: Markup,
    heap: Markup,
    transcript: Markup,
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
                        span id="conn" class="conn ok" { "● live" }
                    }
                    div class="pane tree" {
                        h2 { "cognition tree" }
                        form class="new-root" data-on-submit="@post('/create')"
                            style="display:flex;flex-direction:column;gap:.4rem;margin-bottom:.8rem" {
                            input type="text" name="title" data-bind="title"
                                placeholder="title (optional)"
                                style="width:100%;padding:.4rem;font:inherit";
                            textarea name="prompt" data-bind="prompt" rows="3"
                                placeholder="prompt for the root turn…"
                                style="width:100%;padding:.4rem;font:inherit;resize:vertical" {}
                            button type="submit" class="ghost" { "＋ create root" }
                        }
                        div id="tree" { (tree) }
                    }
                    div class="pane main" {
                        div class="sub sub-inspector" {
                            h2 { "inspector" }
                            div id="inspector" { (inspector) }
                        }
                        div class="sub sub-transcript" {
                            h2 { "transcript" }
                            div id="transcript" { (transcript) }
                        }
                    }
                    div class="pane side" {
                        div class="sub" {
                            h2 { "meters" }
                            div id="meters" { (meters) }
                        }
                        // trace + heap are usually empty on the live path (effect
                        // tracing is a reserved wire slot; sessions are transient),
                        // so they collapse to one-liners rather than eat a pane.
                        details class="sub sub-collapse" {
                            summary { "trace" }
                            div id="trace" { (trace) }
                        }
                        details class="sub sub-collapse" {
                            summary { "heap" }
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
    html! { div class="empty" { "Nothing needs an answer. Start a node, or wait for one to ask." } }
}
