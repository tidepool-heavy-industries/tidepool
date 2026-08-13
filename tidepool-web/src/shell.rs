//! The full HTML document: inline CSS (Swiss / International Typographic
//! Style) + a small vendored vanilla-JS Datastar client. No CDN, no external
//! font, no build step — everything ships in one served page.
//!
//! The client ([`JS`]) speaks exactly the wire the renderer emits: it opens
//! the `/sse` stream, applies `datastar-patch-elements` frames by replacing
//! the same-`id` element in place, and wires `data-on-submit`/
//! `data-on-click="@post('/x')"` handlers. A `data-on-submit` form collects
//! every `[data-bind]` input into a FLAT `{ <key>: <scalar> }` object
//! (coerced by `data-kind`: int → number, bool → boolean, enum/text →
//! string) and POSTs it.
//!
//! ## Focus-preserving skip is gated on `data-rev` (F10)
//! A focused/typed-in field is preserved across an SSE tick ONLY when the
//! incoming fragment's `data-rev` (stamped by [`crate::render::panel`])
//! matches the currently-mounted element's — i.e. the server re-rendered the
//! SAME pending interaction (e.g. a periodic keep-alive tick). A DIFFERENT
//! `data-rev` always replaces the element regardless of focus: it means the
//! pending interaction itself changed (a submit resolved a form and the next
//! interaction — `Idle`, another form, the continue gate — was published),
//! and skipping that replace on stale-focus grounds is exactly the bug this
//! gate fixes (a submit's resulting SSE tick used to get dropped while the
//! panel still had focus, leaving the operator staring at an already-resolved
//! form).

use maud::{html, Markup, PreEscaped, DOCTYPE};

/// The full page: `<head>` with inline [`CSS`] + [`JS`], `<body>` with a
/// masthead and the passed `panel` markup (already `id="panel"`).
pub fn page(panel: Markup) -> Markup {
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
                    (panel)
                }
            }
        }
    }
}

/// Award-grade Swiss / International Typographic Style stylesheet. One
/// spacing unit, a three-step type scale in a fixed ratio, hairlines as the
/// only delimiters, a single scarce accent spent on exactly one thing (the
/// primary action). Every native control is restyled — square, flat, no
/// browser chrome — so the page reads as one composed sheet, not a form.
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

/* Last turn's Haskell — a readable code sheet: preserved line structure,
   soft-wrapped long strings (prompts/notes inside the code would otherwise
   run far off-canvas), hairline frame in the page's print idiom. */
.continue-input {
  display: block;
  width: 100%;
  margin: var(--unit) 0;
  padding: var(--unit);
  border: var(--hair-faint);
  background: transparent;
  font: inherit;
  font-size: var(--text-body);
  resize: vertical;
}

.turn-source { margin-top: calc(var(--unit) * 2); }
.turn-source summary {
  font-size: var(--text-micro);
  letter-spacing: var(--tracking-wide);
  text-transform: uppercase;
  color: var(--muted);
  cursor: pointer;
}
.turn-source pre {
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
  margin-bottom: calc(7 * var(--unit));
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
  display: flex; flex-direction: column; align-items: center; gap: calc(4 * var(--unit));
  text-align: center;
  padding: calc(10 * var(--unit)) 0;
  border-bottom: var(--hair);
}
.continue .eyebrow { color: var(--muted); }
.continue .btn-primary { padding: calc(2.5 * var(--unit)) calc(6 * var(--unit)); }

/* ---------------------------------------------------------------------- idle */
.idle {
  display: flex; flex-direction: column; align-items: center; gap: calc(3 * var(--unit));
  text-align: center;
  padding: calc(11 * var(--unit)) 0;
  border-bottom: var(--hair);
}
.idle .eyebrow { color: var(--muted); }
.idle-glyph {
  font-size: var(--text-display); font-weight: 300; line-height: 1; color: var(--line-faint);
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
/// by same-`id` replacement, and collects a `data-on-submit` form into a FLAT,
/// `data-kind`-coerced submission for the two-verb (`/submit`, `/continue`)
/// surface.
pub const JS: &str = r#"
(function () {
  // Apply one datastar-patch-elements payload: replace each same-id element in
  // place. A focused/typed-in field is preserved (its element is left this
  // tick) ONLY when the incoming data-rev matches the currently-mounted
  // element's — the SAME pending interaction re-rendered. A DIFFERENT
  // data-rev (a new pending interaction was published) always replaces the
  // element regardless of focus, so a submit's resulting tick is never
  // dropped just because the panel still has focus.
  function applyPatch(html) {
    const tpl = document.createElement('template');
    tpl.innerHTML = html.trim();
    tpl.content.querySelectorAll('[id]').forEach((next) => {
      const cur = document.getElementById(next.id);
      if (!cur) { document.body.appendChild(next); wire(next); return; }
      const sameRev = cur.getAttribute('data-rev') === next.getAttribute('data-rev');
      const active = document.activeElement;
      if (sameRev && active && active !== document.body && cur.contains(active)) return;
      cur.replaceWith(next);
      wire(next);
    });
  }

  // Wire data-on-* handlers within a root (idempotent via __wired).
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
        post(parsePost(form.getAttribute('data-on-submit')), form, collect(form), form);
      });
    });
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
    fn page_embeds_panel_and_inline_assets() {
        let doc = page(html! { div id="panel" { "hi" } }).into_string();
        assert!(doc.contains("<div id=\"panel\">hi</div>"));
        // inline, no external host
        assert!(doc.contains("<style>"));
        assert!(doc.contains("<script>"));
        assert!(!doc.contains("http://"));
        assert!(!doc.contains("https://"));
        assert!(!doc.contains("cdn"));
    }

    #[test]
    fn js_collects_flat_and_opens_sse() {
        assert!(JS.contains("new EventSource('/sse')"));
        assert!(JS.contains("data-bind"));
        assert!(JS.contains("datastar-patch-elements"));
    }

    /// F10: the focus-preserving skip must be GATED on a matching `data-rev`
    /// — computed and checked before the unconditional replace, so a
    /// differing revision (a new pending interaction) always reaches
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
