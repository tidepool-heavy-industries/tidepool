//! The full HTML document: inline CSS (Swiss / International Typographic
//! Style) + a small vendored vanilla-JS Datastar client. No CDN, no external
//! font, no build step — everything ships in one served page.
//!
//! The client ([`JS`]) speaks exactly the wire the renderer emits: it opens
//! the `/sse` stream, applies `datastar-patch-elements` frames by replacing
//! the same-`id` element in place (preserving a focused/typed-in field across
//! ticks), and wires `data-on-submit`/`data-on-click="@post('/x')"` handlers.
//! A `data-on-submit` form collects every `[data-bind]` input into a FLAT
//! `{ <key>: <scalar> }` object (coerced by `data-kind`: int → number, bool →
//! boolean, enum/text → string) and POSTs it.

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
                        span class="mark" { "tidepool" }
                        span id="conn" class="conn ok" { "live" }
                    }
                    (panel)
                }
            }
        }
    }
}

/// Baseline Swiss-minimal stylesheet. Off-white ground, single accent, hairline
/// rules, one type family, a small deliberate scale. (Design deliverable — the
/// aesthetic leaf elevates this to the award-grade bar.)
pub const CSS: &str = r#"
:root {
  --paper: #f4f2ec;
  --ink: #16150f;
  --muted: #6f6b60;
  --rule: #16150f;
  --accent: #c8341e;
  --hair: 1px solid var(--rule);
  --unit: 8px;
}
* { box-sizing: border-box; }
html, body { margin: 0; background: var(--paper); color: var(--ink); }
body {
  font-family: ui-sans-serif, "Helvetica Neue", Helvetica, Arial, sans-serif;
  font-size: 16px; line-height: 1.5;
  -webkit-font-smoothing: antialiased;
}
.sheet { max-width: 640px; margin: 0 auto; padding: calc(6 * var(--unit)) calc(3 * var(--unit)); }
.masthead {
  display: flex; justify-content: space-between; align-items: baseline;
  border-bottom: var(--hair); padding-bottom: var(--unit); margin-bottom: calc(4 * var(--unit));
}
.mark { font-weight: 700; letter-spacing: 0.02em; }
.conn { font-size: 11px; text-transform: uppercase; letter-spacing: 0.12em; color: var(--muted); }
.conn.down { color: var(--accent); }
.eyebrow {
  font-size: 11px; text-transform: uppercase; letter-spacing: 0.14em;
  color: var(--muted); margin: 0 0 var(--unit) 0;
}
.field { border-top: var(--hair); padding: calc(2 * var(--unit)) 0; }
.field:first-child { border-top: none; }
.input {
  width: 100%; font: inherit; padding: var(--unit); background: transparent;
  border: var(--hair); color: var(--ink);
}
.enum { display: flex; flex-direction: column; gap: var(--unit); }
.enum-opt, .bool { display: flex; align-items: center; gap: var(--unit); cursor: pointer; }
.actions { border-top: var(--hair); padding-top: calc(2 * var(--unit)); margin-top: calc(2 * var(--unit)); }
.btn {
  font: inherit; font-weight: 600; padding: var(--unit) calc(3 * var(--unit));
  border: var(--hair); background: transparent; color: var(--ink); cursor: pointer;
  letter-spacing: 0.02em;
}
.btn-primary { background: var(--accent); color: var(--paper); border-color: var(--accent); }
.btn[disabled] { opacity: 0.5; cursor: default; }
.idle-note, .continue { color: var(--muted); }
#toast {
  position: fixed; right: calc(2 * var(--unit)); bottom: calc(2 * var(--unit));
  max-width: 320px; padding: var(--unit) calc(2 * var(--unit)); border: var(--hair);
  background: var(--paper); font-size: 13px; display: none;
}
#toast.show { display: block; }
#toast.err { border-color: var(--accent); color: var(--accent); }
"#;

/// The vendored Datastar client. Lifted from the observatory's patch-apply +
/// form-collection JS, trimmed to the two-verb form surface and a FLAT,
/// type-coerced submission.
pub const JS: &str = r#"
(function () {
  // Apply one datastar-patch-elements payload: replace each same-id element in
  // place. A focused/typed-in field is preserved (its element is left this tick)
  // so a live SSE tick never rips out what the operator is doing.
  function applyPatch(html) {
    const tpl = document.createElement('template');
    tpl.innerHTML = html.trim();
    tpl.content.querySelectorAll('[id]').forEach((next) => {
      const cur = document.getElementById(next.id);
      if (!cur) { document.body.appendChild(next); wire(next); return; }
      const active = document.activeElement;
      if (active && active !== document.body && cur.contains(active)) return;
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
}
