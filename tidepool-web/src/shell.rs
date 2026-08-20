//! The full HTML document: inline CSS (Swiss / International Typographic
//! Style) + a small vendored vanilla-JS Datastar client. No CDN, no external
//! font, no build step — everything ships in one served page.
//!
//! The page is ONE OUTLINE: every registered node renders as its own section,
//! always visible, indented by tree depth (slash count in the `node_id`),
//! sorted by path with the default node pinned first. There are no tabs —
//! collapsing a section (the header toggle) is the only hiding mechanism,
//! and it is operator-initiated. The collapse class lives on the STABLE
//! `.node-slot` wrapper, never on the SSE-patched inner `#panel-<node_id>`,
//! so an operator's toggle survives any number of live patches.
//!
//! The client ([`JS`]) speaks exactly the wire the renderer emits: it opens
//! the `/sse` stream, applies `datastar-patch-elements` frames by replacing
//! the same-`id` element in place, and — when a frame carries a panel the
//! page has NEVER seen (a node born after page load) — MOUNTS it into
//! `#tree` at its sorted position inside a freshly built `.node-slot`
//! wrapper, so the operator watches the tree grow live. A `data-on-submit`
//! form collects every `[data-bind]` input into a FLAT `{ <key>: <scalar> }`
//! object (coerced by `data-kind`: int → number, bool → boolean, enum/text →
//! string) and POSTs it to the node/interaction-scoped URL baked into the
//! form's `@post(...)` literal.
//!
//! ## Focus-preserving skip is gated on `data-rev` (F10, per-node)
//! A focused/typed-in field is preserved across an SSE tick ONLY when the
//! incoming fragment's `data-rev` (stamped by [`crate::render::node_panel`]
//! on that node's panel root) matches the currently-mounted element's — i.e.
//! the server re-rendered the SAME node's state. A DIFFERENT `data-rev`
//! always replaces the element regardless of focus: it means THIS node's
//! state itself changed, and skipping that replace on stale-focus grounds is
//! exactly the bug this gate fixes.

use maud::{html, Markup, PreEscaped, DOCTYPE};

/// The full page: `<head>` with inline [`CSS`] + [`JS`], `<body>` with a
/// masthead and the `#tree` outline — one stable `.node-slot` wrapper per
/// `(node_id, panel)` pair, in the caller's (already display-sorted) order.
///
/// `node_id`s are slash-separated paths (`root/1-execution-mode`) — each
/// slot indents by path depth, so the outline reads as a tree. The wrapper
/// for [`crate::DEFAULT_NODE_ID`] carries `data-pinned`, which the client's
/// mount-on-first-sight insert uses to keep it first regardless of sort.
pub fn page(panels: Vec<(String, Markup)>) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "tidepool — operator" }
                style { (PreEscaped(CSS)) }
                script { (PreEscaped(JS)) }
            }
            body {
                main class="sheet" {
                    header class="masthead" {
                        div class="mast-title" {
                            span class="mark" { "tidepool" }
                            span class="mast-sub" { "self-iterating harness — operator console" }
                        }
                        span id="conn" class="conn ok" { "live" }
                    }
                    div id="tree" data-node="tree" {
                        @for (node_id, panel) in &panels {
                            div class="node-slot"
                                data-node-id=(node_id)
                                data-pinned[node_id == crate::DEFAULT_NODE_ID]
                                style=(slot_indent_style(node_id)) {
                                (panel)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A node's tree depth — the slash count in its `node_id` (`"root"` → 0,
/// `"root/1-x"` → 1, `"root/1-x/2-y"` → 2). Purely a function of the id
/// string, so no separate depth field needs to ride alongside it anywhere.
/// The client computes the same depth for panels it mounts itself.
fn path_depth(node_id: &str) -> usize {
    node_id.matches('/').count()
}

/// Inline left margin proportional to [`path_depth`] — an unbounded tree
/// depth can't be covered by a fixed set of `[data-depth="N"]` CSS rules, so
/// this is computed per slot. The client's mount path uses the same
/// `depth * 14` formula.
fn slot_indent_style(node_id: &str) -> String {
    format!("margin-left: {}px", path_depth(node_id) * 14)
}

/// Award-grade Swiss / International Typographic Style stylesheet. One
/// spacing unit, a three-step type scale in a fixed ratio, hairlines as the
/// only delimiters, a single scarce accent spent on the places the operator
/// is needed. Every native control is restyled — square, flat, no browser
/// chrome — so the page reads as one composed sheet, not a form.
pub const CSS: &str = r#"
:root {
  --paper: #f5f3ec;
  --ink: #16150f;
  --muted: #78725f;
  --line: #16150f;
  --line-faint: #d8d3c4;
  --accent: #c8341e;
  --hair: 1px solid var(--line);
  --hair-faint: 1px solid var(--line-faint);
  --unit: 8px;

  --text-micro: 0.6875rem;  /* 11px — eyebrows, meta */
  --text-body: 1rem;        /* 16px — field values, prose */
  --text-display: 2.5rem;   /* 40px — masthead, standby glyph */
  --tracking-wide: 0.14em;
}

* { box-sizing: border-box; }

html, body { margin: 0; background: var(--paper); }
body {
  color: var(--ink);
  font-family: ui-sans-serif, "Helvetica Neue", Helvetica, Arial, sans-serif;
  font-size: var(--text-body); line-height: 1.5;
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
}

.sheet {
  max-width: 720px; margin: 0 auto;
  padding: calc(7 * var(--unit)) calc(4 * var(--unit)) calc(12 * var(--unit));
}

/* ---------------------------------------------------------------- masthead */
.masthead {
  display: grid; grid-template-columns: 1fr auto; align-items: end;
  column-gap: calc(3 * var(--unit));
  padding-bottom: calc(3 * var(--unit));
  margin-bottom: calc(4 * var(--unit));
  border-bottom: var(--hair);
}
.mast-title { display: flex; flex-direction: column; gap: var(--unit); }
.mark {
  font-size: var(--text-display); font-weight: 700;
  letter-spacing: -0.02em; line-height: 1;
}
.mast-sub {
  font-size: var(--text-micro); font-weight: 600; text-transform: uppercase;
  letter-spacing: var(--tracking-wide); color: var(--muted);
}
.conn {
  font-size: var(--text-micro); font-weight: 600; text-transform: uppercase;
  letter-spacing: var(--tracking-wide); color: var(--muted);
  padding-bottom: 0.2em;
}
.conn.down { color: var(--ink); text-decoration: underline; text-underline-offset: 0.2em; }

/* -------------------------------------------------------------- typography */
.eyebrow {
  font-size: var(--text-micro); font-weight: 700; text-transform: uppercase;
  letter-spacing: var(--tracking-wide); color: var(--ink); margin: 0;
}

/* ---------------------------------------------------------- node sections */
.node-slot {
  padding: calc(3 * var(--unit)) 0;
  border-bottom: var(--hair-faint);
}
.node-head {
  display: flex; align-items: baseline; gap: calc(2 * var(--unit));
}
.node-toggle {
  font: inherit; font-size: 0.75rem; line-height: 1;
  border: none; background: transparent; color: var(--muted);
  cursor: pointer; padding: 0; flex: none;
  transition: transform 0.12s ease;
}
.node-slot.collapsed .node-toggle { transform: rotate(-90deg); }
.node-slot.collapsed .node-body { display: none; }
.node-title {
  font-size: var(--text-body); font-weight: 700; margin: 0;
  letter-spacing: -0.01em; overflow-wrap: anywhere;
}
.node-panel.done .node-title, .node-panel.ended .node-title { color: var(--muted); }
.status {
  margin-left: auto; flex: none;
  font-size: var(--text-micro); font-weight: 700; text-transform: uppercase;
  letter-spacing: var(--tracking-wide);
  padding: calc(0.5 * var(--unit)) calc(1.5 * var(--unit));
}
.status.needs-you { background: var(--accent); color: var(--paper); }
.status.running { border: 1px solid var(--ink); color: var(--ink); }
.status.done, .status.ended { border: 1px solid var(--line-faint); color: var(--muted); }
.status.failed { border: 1px solid var(--accent); color: var(--accent); }

.node-body { padding-top: calc(2 * var(--unit)); }

/* ----------------------------------------------------------- node content */
.timeline { display: flex; flex-direction: column; gap: calc(2 * var(--unit)); }
.note { margin: 0; }

.seed summary, .turn-source summary {
  font-size: var(--text-micro);
  letter-spacing: var(--tracking-wide);
  text-transform: uppercase;
  color: var(--muted);
  cursor: pointer;
}
.seed { margin-bottom: calc(2 * var(--unit)); }

/* mono blocks — seeds, answers, final values, failures, turn sources: one
   readable code-sheet idiom, preserved line structure, soft-wrapped. */
.seed pre, .answered pre, .final pre, .failure pre, .turn-source pre {
  margin: var(--unit) 0 0 0;
  padding: calc(var(--unit) * 1.5);
  border: var(--hair-faint);
  background: rgba(22, 21, 15, 0.03);
  font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  font-size: 0.8125rem;
  line-height: 1.5;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  overflow-x: auto;
  max-height: 24rem;
  overflow-y: auto;
}

.answered {
  border-left: 2px solid var(--line-faint);
  padding-left: calc(2 * var(--unit));
}
.answered .eyebrow { color: var(--muted); }
.answered pre { border: none; background: transparent; padding: 0; color: var(--muted); }

.final { margin-top: calc(2 * var(--unit)); }
.failure { margin-top: calc(2 * var(--unit)); }
.failure-eyebrow { color: var(--accent); }

.turn-source { margin-top: calc(var(--unit) * 2); }
.turn-history {
  margin-top: var(--unit);
  max-height: 40vh;
  overflow-y: auto;
}
.turn-entry { margin-top: var(--unit); }
.turn-entry summary {
  font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  font-size: 0.75rem;
  color: var(--muted);
  cursor: pointer;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* ------------------------------------------------------------------ form */
.field {
  display: grid; grid-template-columns: calc(11 * var(--unit)) 1fr;
  column-gap: calc(4 * var(--unit));
  padding: calc(4 * var(--unit)) 0;
  border-top: var(--hair-faint);
}
.field:first-child { border-top: none; padding-top: 0; }

.field-meta { display: flex; flex-direction: column; gap: calc(1.5 * var(--unit)); }
.field-index {
  font-size: var(--text-micro); font-variant-numeric: tabular-nums;
  color: var(--muted);
}

.field-input { display: flex; align-items: center; min-height: calc(4.5 * var(--unit)); }

.input {
  width: 100%; font: inherit; font-size: var(--text-body); color: var(--ink);
  background: transparent; border: none; border-bottom: var(--hair);
  padding: calc(1 * var(--unit)) 0; border-radius: 0;
}
.input:focus {
  outline: none; border-bottom: 2px solid var(--ink);
  padding-bottom: calc(1 * var(--unit) - 1px);
}
input.input[data-kind="int"] {
  text-align: right; font-variant-numeric: tabular-nums;
  appearance: textfield; -moz-appearance: textfield;
}
input.input[data-kind="int"]::-webkit-outer-spin-button,
input.input[data-kind="int"]::-webkit-inner-spin-button {
  appearance: none; -webkit-appearance: none; margin: 0;
}

.field-error {
  margin: calc(1 * var(--unit)) 0 0 0;
  color: var(--accent); font-size: var(--text-micro); font-weight: 600;
}
.sum.invalid > .enum { outline: 2px solid var(--accent); outline-offset: calc(1 * var(--unit)); }

.enum { display: flex; flex-direction: column; gap: calc(2 * var(--unit)); width: 100%; }
.enum-opt, .bool {
  display: flex; align-items: center; gap: calc(2 * var(--unit));
  cursor: pointer; font-size: var(--text-body);
}

/* square, flat check controls — no native chrome, no border-radius anywhere.
   The two kinds carry different marks on purpose: an exclusive choice
   (radio) reads as a solid fill; an independent toggle (checkbox) reads as
   a drawn mark inside an unfilled box — two meanings, two marks. */
input[type="radio"], input[type="checkbox"] {
  appearance: none; -webkit-appearance: none; margin: 0; flex: none;
  width: calc(2 * var(--unit)); height: calc(2 * var(--unit));
  border: var(--hair); border-radius: 0; background: var(--paper);
  cursor: pointer; position: relative;
}
input[type="radio"]:checked { background: var(--ink); }
input[type="checkbox"]:checked::after {
  content: ""; position: absolute; inset: calc(0.25 * var(--unit));
  background: var(--ink);
  clip-path: polygon(14% 44%, 0% 63%, 38% 100%, 100% 16%, 78% 0%, 35% 62%);
}
input[type="radio"]:focus-visible, input[type="checkbox"]:focus-visible,
.input:focus-visible, .btn:focus-visible {
  outline: 2px solid var(--ink); outline-offset: 2px;
}

.actions {
  display: flex; justify-content: flex-end;
  border-top: var(--hair); margin-top: calc(3 * var(--unit)); padding-top: calc(4 * var(--unit));
}
.btn {
  font: inherit; font-size: var(--text-micro); font-weight: 700; text-transform: uppercase;
  letter-spacing: var(--tracking-wide);
  padding: calc(2 * var(--unit)) calc(4 * var(--unit));
  border: var(--hair); border-radius: 0; background: transparent; color: var(--ink);
  cursor: pointer;
}
.btn-primary { background: var(--accent); color: var(--paper); border-color: var(--accent); }
.btn[disabled] { opacity: 0.4; cursor: default; }

/* ------------------------------------------------------------------ continue */
.continue {
  display: flex; flex-direction: column; align-items: center; gap: calc(3 * var(--unit));
  text-align: center;
  padding: calc(4 * var(--unit)) 0;
  border-top: var(--hair-faint);
  border-bottom: var(--hair-faint);
}
.continue .eyebrow { color: var(--muted); }
.continue .btn-primary { padding: calc(2.5 * var(--unit)) calc(6 * var(--unit)); }

/* ---------------------------------------------------------------------- idle */
.idle {
  display: flex; flex-direction: column; align-items: center; gap: calc(2 * var(--unit));
  text-align: center;
  padding: calc(5 * var(--unit)) 0;
}
.idle .eyebrow { color: var(--muted); }
.idle-glyph {
  font-size: var(--text-display); font-weight: 300; line-height: 1; color: var(--line-faint);
  margin: 0;
}
.idle-note { margin: 0; color: var(--muted); font-size: var(--text-body); }

/* ---------------------------------------------------------------------- toast */
#toast {
  position: fixed; right: calc(3 * var(--unit)); bottom: calc(3 * var(--unit));
  max-width: 320px; padding: calc(2 * var(--unit)) calc(3 * var(--unit));
  border: var(--hair); border-radius: 0;
  background: var(--paper); font-size: var(--text-micro); letter-spacing: 0.02em;
  display: none;
}
#toast.show { display: block; }
#toast.err { border-width: 2px; font-weight: 600; }

@media (max-width: 520px) {
  .field { grid-template-columns: 1fr; row-gap: calc(2 * var(--unit)); }
}
"#;

/// The vendored Datastar client: opens `/sse`, applies patch-elements frames
/// by same-`id` replacement (one `panel-<node_id>` per registered node) —
/// MOUNTING a never-seen panel into `#tree` at its sorted position —
/// collects a `data-on-submit` form into a FLAT, `data-kind`-coerced
/// submission and POSTs it to the node/interaction-scoped URL already baked
/// into that form's `@post(...)` literal, and wires each section's collapse
/// toggle (the collapse class lives on the stable `.node-slot` wrapper, so
/// it survives patches).
pub const JS: &str = r#"
(function () {
  // Apply one datastar-patch-elements payload: replace each same-id element in
  // place. A focused/typed-in field is preserved (its element is left this
  // tick) ONLY when the incoming data-rev matches the currently-mounted
  // element's — the SAME state re-rendered. A DIFFERENT data-rev (this
  // node's state changed) always replaces the element regardless of focus,
  // so a submit's resulting tick is never dropped just because the panel
  // still has focus. An id the page has never seen is a NEW node — mount it
  // into the tree at its sorted position.
  function applyPatch(html) {
    const tpl = document.createElement('template');
    tpl.innerHTML = html.trim();
    tpl.content.querySelectorAll('[id]').forEach((next) => {
      const cur = document.getElementById(next.id);
      if (!cur) { mountPanel(next); return; }
      const sameRev = cur.getAttribute('data-rev') === next.getAttribute('data-rev');
      const active = document.activeElement;
      if (sameRev && active && active !== document.body && cur.contains(active)) return;
      cur.replaceWith(next);
      wire(next);
    });
  }

  // Mount a panel this page has never seen (a node born after page load):
  // build the stable .node-slot wrapper the initial render would have built
  // (indent = path depth * 14px, matching the server), and insert it into
  // #tree at its path-sorted position — pinned slots (the default node)
  // always stay first.
  function mountPanel(next) {
    if (!/^panel-/.test(next.id)) return;
    const tree = document.getElementById('tree');
    if (!tree) return;
    const path = next.getAttribute('data-path') || next.id.slice(6);
    const slot = document.createElement('div');
    slot.className = 'node-slot';
    slot.setAttribute('data-node-id', path);
    const depth = (path.match(/\//g) || []).length;
    slot.style.marginLeft = (depth * 14) + 'px';
    slot.appendChild(next);
    const siblings = Array.from(tree.querySelectorAll(':scope > .node-slot'));
    const after = siblings.find((s) => !s.hasAttribute('data-pinned')
      && s.getAttribute('data-node-id') > path);
    tree.insertBefore(slot, after || null);
    wire(slot);
  }

  // Wire data-on-* handlers and collapse toggles within a root (idempotent
  // via __wired).
  function wire(root) {
    root.querySelectorAll('[data-on-click]').forEach((el) => {
      if (el.__wired) return; el.__wired = true;
      el.addEventListener('click', (e) => {
        e.preventDefault();
        post(parsePost(el.getAttribute('data-on-click')), el, null, null);
      });
    });
    root.querySelectorAll('[data-on-submit]').forEach((form) => {
      if (form.__wired) return; form.__wired = true;
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        if (!validateRequired(form)) return;
        post(parsePost(form.getAttribute('data-on-submit')), form, collect(form), form);
      });
    });
    // The collapse toggle flips a class on the STABLE .node-slot wrapper —
    // never on the patched panel — so the operator's choice survives any
    // number of SSE patches to the panel inside.
    root.querySelectorAll('[data-toggle]').forEach((btn) => {
      if (btn.__wired) return; btn.__wired = true;
      btn.addEventListener('click', () => {
        const slot = btn.closest('.node-slot');
        if (slot) slot.classList.toggle('collapsed');
      });
    });
  }

  // An element hidden by the payload-sum CSS reveal (or any display:none
  // ancestor) has no box — the standard offsetParent-null test.
  function isVisible(el) {
    return !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);
  }

  // A field inside an unchecked Optional's `.optional-inner` is deliberately
  // excluded (collect_form_json never reads it — Optional submits `null`
  // without looking at the inner shape at all when `#present` is unchecked),
  // so it must never be treated as required even though `.optional-inner` has
  // no CSS hiding of its own (unlike a payload-sum's `.variant-payload`).
  // Walks every ancestor `.optional` (nested Maybes), not just the nearest.
  function enabledByAncestors(el) {
    let node = el.closest('.optional');
    while (node) {
      const toggle = node.querySelector('.optional-toggle input[data-bind]');
      if (toggle && !toggle.checked) return false;
      node = node.parentElement ? node.parentElement.closest('.optional') : null;
    }
    return true;
  }

  // Block submit when a VISIBLE, ENABLED radio/enum group has no option
  // checked, showing an inline message next to it instead of silently
  // submitting without that key — an empty gate submission decode-fails
  // Haskell-side and used to silently re-present the same form (three
  // consecutive silent re-prompt loops, observed live). A hidden group (a
  // payload-sum branch the operator did not choose) or one inside an
  // unchecked Optional is never required — collect() never reads either.
  function validateRequired(form) {
    form.querySelectorAll('.field-error').forEach((el) => el.remove());
    form.querySelectorAll('[data-node="sum"]').forEach((el) => el.classList.remove('invalid'));

    const seen = new Set();
    let ok = true;
    form.querySelectorAll('input[data-kind="enum"]').forEach((f) => {
      const key = f.getAttribute('data-bind');
      if (seen.has(key) || !isVisible(f) || !enabledByAncestors(f)) return;
      seen.add(key);
      const group = Array.from(form.querySelectorAll('input[data-kind="enum"]'))
        .filter((g) => g.getAttribute('data-bind') === key);
      if (group.some((g) => g.checked)) return;
      ok = false;
      const host = f.closest('[data-node="sum"]') || f.closest('.field') || form;
      host.classList.add('invalid');
      const msg = document.createElement('p');
      msg.className = 'field-error';
      msg.textContent = 'Choose one — this field is required.';
      host.appendChild(msg);
    });
    return ok;
  }

  // Collect every [data-bind] input into a FLAT { key: scalar }, coercing by
  // data-kind: int -> number, bool -> boolean, enum/text -> string. A radio
  // group shares one key; only the checked option contributes.
  function collect(form) {
    const body = {};
    form.querySelectorAll('[data-bind]').forEach((f) => {
      const key = f.getAttribute('data-bind');
      const kind = f.getAttribute('data-kind');
      if (kind === 'bool') { body[key] = !!f.checked; return; }
      if (f.type === 'radio') { if (f.checked) body[key] = f.value; return; }
      if (kind === 'int') {
        const n = f.value.trim();
        body[key] = n === '' ? null : Number(n);
        return;
      }
      body[key] = f.value;
    });
    return body;
  }

  function parsePost(expr) {
    const m = /@post\(\s*'([^']*)'\s*\)/.exec(expr || '');
    return m ? m[1] : null;
  }

  // POST a verb: in-flight disabled button, then a toast on failure (the server
  // sends {ok:false,error} with a 4xx; fetch does not reject on those).
  async function post(url, el, body, form) {
    if (!url) return;
    const btn = (el && el.tagName === 'BUTTON') ? el
              : (form ? form.querySelector('button[type=submit]') : null);
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
      }
    } catch (e) {
      toast('network error — is the server up? (' + e.message + ')', true);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  function toast(msg, isErr) {
    let t = document.getElementById('toast');
    if (!t) { t = document.createElement('div'); t.id = 'toast'; document.body.appendChild(t); }
    if (!msg) { t.classList.remove('show'); return; }
    t.className = (isErr ? 'err ' : '') + 'show';
    t.textContent = msg;
    clearTimeout(t.__timer);
    t.__timer = setTimeout(() => t.classList.remove('show'), 7000);
  }

  function setConn(ok) {
    const s = document.getElementById('conn');
    if (!s) return;
    s.className = 'conn ' + (ok ? 'ok' : 'down');
    s.textContent = ok ? 'live' : 'reconnecting';
  }

  function connect() {
    const es = new EventSource('/sse');
    es.onopen = () => setConn(true);
    es.addEventListener('datastar-patch-elements', (ev) => {
      const html = ev.data.split('\n')
        .map((l) => l.replace(/^elements /, ''))
        .join('\n');
      applyPatch(html);
    });
    es.onerror = () => setConn(false);
  }

  window.addEventListener('DOMContentLoaded', () => { wire(document); connect(); });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use maud::html;

    #[test]
    fn page_embeds_panels_and_inline_assets() {
        let doc = page(vec![(
            "n1".to_string(),
            html! { div id="panel-n1" { "hi" } },
        )])
        .into_string();
        assert!(doc.contains("<div id=\"panel-n1\">hi</div>"));
        // inline, no external host
        assert!(doc.contains("<style>"));
        assert!(doc.contains("<script>"));
        assert!(!doc.contains("http://"));
        assert!(!doc.contains("https://"));
        assert!(!doc.contains("cdn"));
    }

    /// Every node renders as its own always-visible section inside a stable
    /// `.node-slot` wrapper — no tabs, nothing hidden by default.
    #[test]
    fn every_node_renders_as_a_visible_section() {
        let doc = page(vec![
            ("alpha".to_string(), html! { div id="panel-alpha" {} }),
            ("beta".to_string(), html! { div id="panel-beta" {} }),
        ])
        .into_string();
        assert!(doc.contains("data-node-id=\"alpha\""), "{doc}");
        assert!(doc.contains("data-node-id=\"beta\""), "{doc}");
        assert!(doc.contains("id=\"tree\""), "{doc}");
        assert!(!doc.contains("data-tab="), "no tabs anywhere: {doc}");
    }

    /// The default node's slot carries `data-pinned` (the client's
    /// mount-on-first-sight insert keeps pinned slots first); other nodes'
    /// slots don't.
    #[test]
    fn default_node_slot_is_pinned() {
        let doc = page(vec![
            (
                crate::DEFAULT_NODE_ID.to_string(),
                html! { div id="panel-root" {} },
            ),
            ("root/1-x".to_string(), html! { div id="panel-root/1-x" {} }),
        ])
        .into_string();
        // Assert on the slot markup itself ("data-pinned" also appears inside
        // the embedded JS, which handles it on the mount path).
        assert!(
            doc.contains(&format!(
                "data-node-id=\"{}\" data-pinned",
                crate::DEFAULT_NODE_ID
            )),
            "{doc}"
        );
        assert!(
            !doc.contains("data-node-id=\"root/1-x\" data-pinned"),
            "{doc}"
        );
    }

    /// Slash-separated `node_id`s indent by path depth — the outline reads
    /// as a tree.
    #[test]
    fn slots_indent_by_path_depth() {
        let doc = page(vec![
            ("root".to_string(), html! { div id="panel-root" {} }),
            ("root/1-x".to_string(), html! { div id="panel-root/1-x" {} }),
            (
                "root/1-x/2-y".to_string(),
                html! { div id="panel-root/1-x/2-y" {} },
            ),
        ])
        .into_string();

        let root_style = slot_indent_style("root");
        let mid_style = slot_indent_style("root/1-x");
        let leaf_style = slot_indent_style("root/1-x/2-y");
        assert_ne!(root_style, mid_style);
        assert_ne!(mid_style, leaf_style);
        assert!(doc.contains(&format!("style=\"{root_style}\"")), "{doc}");
        assert!(doc.contains(&format!("style=\"{mid_style}\"")), "{doc}");
        assert!(doc.contains(&format!("style=\"{leaf_style}\"")), "{doc}");
    }

    #[test]
    fn js_collects_flat_and_opens_sse() {
        assert!(JS.contains("new EventSource('/sse')"));
        assert!(JS.contains("data-bind"));
        assert!(JS.contains("datastar-patch-elements"));
    }

    /// A panel the page has never seen must be MOUNTED into #tree at its
    /// sorted position (pinned slots first) — not appended to document.body
    /// (the old orphaned-append bug: a node born after page load rendered
    /// outside the layout entirely).
    #[test]
    fn js_mounts_unknown_panels_into_the_tree_sorted() {
        assert!(JS.contains("function mountPanel"));
        assert!(
            !JS.contains("document.body.appendChild(next)"),
            "an unknown panel must never be orphan-appended to body"
        );
        assert!(JS.contains("getElementById('tree')"));
        assert!(JS.contains("data-pinned"));
        assert!(JS.contains("tree.insertBefore(slot, after || null)"));
    }

    /// The collapse toggle flips a class on the STABLE `.node-slot` wrapper,
    /// never the SSE-patched inner panel — so an operator's collapse
    /// survives any number of patches.
    #[test]
    fn js_collapse_lives_on_the_stable_wrapper() {
        assert!(JS.contains("[data-toggle]"));
        assert!(JS.contains("btn.closest('.node-slot')"));
        assert!(JS.contains("slot.classList.toggle('collapsed')"));
    }

    /// A required (visible, unselected) radio/enum group must block the
    /// submit's `post(...)` call — the validation gate has to run and return
    /// before `post` is reached, not after or in parallel.
    #[test]
    fn js_submit_is_gated_on_validate_required() {
        assert!(JS.contains("function validateRequired"));
        let gate_idx = JS
            .find("if (!validateRequired(form)) return;")
            .expect("submit is gated on validateRequired");
        let post_idx = JS
            .find(
                "post(parsePost(form.getAttribute('data-on-submit')), form, collect(form), form);",
            )
            .expect("the submit post call exists");
        assert!(
            gate_idx < post_idx,
            "the validateRequired gate must precede the post call"
        );
    }

    /// A required-enum check inside an unchecked `Optional`'s
    /// `.optional-inner` must never block submit — `collect_form_json` never
    /// even reads that inner shape when `#present` is unchecked, so requiring
    /// it would be requiring a field that isn't part of the answer at all.
    #[test]
    fn js_validate_required_skips_fields_disabled_by_an_unchecked_optional() {
        assert!(JS.contains("function enabledByAncestors"));
        let skip_idx = JS
            .find("!isVisible(f) || !enabledByAncestors(f)")
            .expect("the required check consults enabledByAncestors");
        let validate_idx = JS
            .find("function validateRequired")
            .expect("validateRequired exists");
        assert!(
            validate_idx < skip_idx,
            "the check lives inside validateRequired"
        );
    }

    /// F10: the focus-preserving skip must be GATED on a matching `data-rev`
    /// — computed and checked before the unconditional replace, so a
    /// differing revision (this node's state changed) always reaches
    /// `replaceWith` regardless of what currently has focus.
    #[test]
    fn js_focus_skip_gated_on_matching_data_rev() {
        assert!(JS.contains("data-rev"));

        let same_rev_idx = JS.find("const sameRev").expect("sameRev is computed");
        let active_idx = JS
            .find("const active = document.activeElement")
            .expect("active is computed");
        assert!(
            same_rev_idx < active_idx,
            "sameRev must be computed before the focus check reads document.activeElement"
        );

        let gate_idx = JS
            .find("if (sameRev && active")
            .expect("the skip is gated on sameRev");
        let replace_idx = JS
            .find("cur.replaceWith(next)")
            .expect("the unconditional replace exists");
        assert!(
            gate_idx < replace_idx,
            "the sameRev-gated early return must precede the unconditional replace"
        );
    }
}
