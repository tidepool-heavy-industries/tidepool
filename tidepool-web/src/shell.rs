//! `/legacy`'s full HTML document: inline CSS (Swiss / International
//! Typographic Style) + [`CORE_JS`] (the wire contract's client half — see
//! this crate's `CLAUDE.md`, shared verbatim with the d3 tree view) + [`JS`]
//! (this page's own SSE-apply glue). No CDN, no external font, no build step.
//!
//! The page is ONE OUTLINE: every registered node renders as its own section,
//! always visible, indented by tree depth (slash count in the `node_id`),
//! sorted by path with the default node pinned first. There are no tabs —
//! collapsing a section (the header toggle) is the only hiding mechanism,
//! and it is operator-initiated. The collapse class lives on the STABLE
//! `.node-slot` wrapper, never on the SSE-patched inner `#panel-<node_id>`,
//! so an operator's toggle survives any number of live patches.

use maud::{html, Markup, PreEscaped, DOCTYPE};

/// The full page: `<head>` with inline [`CSS`] + [`JS`], `<body>` with a
/// masthead and the `#tree` outline — one stable `.node-slot` wrapper per
/// `(node_id, panel)` pair, in the caller's (already display-sorted) order.
///
/// `node_id`s are slash-separated paths (`root/1-execution-mode`) — each
/// slot indents by path depth, so the outline reads as a tree. The wrapper
/// for [`crate::DEFAULT_NODE_ID`] carries `data-pinned`, which the client's
/// mount-on-first-sight insert uses to keep it first regardless of sort.
///
/// `run_id` — the harness's own run identity, when the boot path has set one
/// (see [`crate::AppState::set_run_id`]) — renders as small masthead text so
/// a pre/post-restart run is distinguishable in a stale tab. `None` renders
/// no masthead text at all, the same page every caller saw before this
/// existed. It is a substrate identifier, never model prose, but is still
/// rendered as an ordinary escaped text node like everything else here.
pub fn page(panels: Vec<(String, Markup)>, run_id: Option<&str>) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "tidepool — operator" }
                style { (PreEscaped(CSS)) }
                script { (PreEscaped(CORE_JS)) }
                script { (PreEscaped(JS)) }
            }
            body {
                main class="sheet" {
                    header class="masthead" {
                        div class="mast-title" {
                            span class="mark" { "tidepool" }
                            span class="mast-sub" { "self-iterating harness — operator console" }
                            @if let Some(id) = run_id {
                                span class="run-id" data-node="run-id" { "run " (id) }
                            }
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

/// Swiss / International Typographic Style stylesheet: one spacing unit, a
/// three-step type scale in a fixed ratio, hairlines as the only delimiters,
/// a single scarce accent spent on the places the operator is needed. Every
/// native control is restyled — square, flat, no browser chrome — so the
/// page reads as one composed sheet, not a form.
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
.run-id {
  font-size: var(--text-micro); font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  color: var(--muted);
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

/* structured finalized/failure values — a definition list, one row per
   field, in place of a raw JSON blob. */
.con-badge {
  display: inline-block; margin-top: var(--unit);
  font-size: var(--text-micro); font-weight: 700; text-transform: uppercase;
  letter-spacing: var(--tracking-wide);
  padding: calc(0.5 * var(--unit)) calc(1.5 * var(--unit));
  border: 1px solid var(--ink); color: var(--ink);
}
.structured { margin: calc(2 * var(--unit)) 0 0 0; }
.structured-row {
  padding: calc(2 * var(--unit)) 0;
  border-top: var(--hair-faint);
}
.structured-row:first-child { border-top: none; padding-top: calc(1 * var(--unit)); }
.structured-row dt { color: var(--muted); }
.structured-row dd {
  margin: calc(1 * var(--unit)) 0 0 0;
}
.prose {
  margin: 0; white-space: pre-wrap; overflow-wrap: anywhere;
  font-size: var(--text-body); line-height: 1.5;
}
.scalar {
  font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  font-size: 0.8125rem;
}
.structured-list { display: flex; flex-direction: column; gap: var(--unit); }

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

/* A payload-bearing sum's non-chosen branches: static, never data-derived —
   `updateSumReveal` (CORE_JS) toggles `.active` on the one whose data-for
   matches the checked radio's value. */
.variant-payload { display: none; }
.variant-payload.active { display: block; }

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

/// The shared vendored client plumbing — form collection/submission, the
/// toast, and the SSE patch-apply/mount machinery — used VERBATIM by both
/// `/legacy`'s outline page ([`JS`], below) and `/` 's d3 tree view
/// ([`crate::tree::TREE_JS`]): one copy of `collect`/`post`/
/// `validateRequired`/`toast` (and the patch-apply pair `applyPatch`/
/// `mountPanel`), never two hand-maintained implementations that could
/// drift apart. Declared as plain top-level functions (no IIFE wrapper) so a
/// page's own script tag, evaluated right after this one in the same
/// classic-script global scope, can call them directly.
pub const CORE_JS: &str = r#"
// Apply one datastar-patch-elements payload: replace each same-id element in
// place. A focused/typed-in field is preserved (its element is left this
// tick) ONLY when the incoming data-rev matches the currently-mounted
// element's — the SAME state re-rendered. A DIFFERENT data-rev (this
// node's state changed) always replaces the element regardless of focus,
// so a submit's resulting tick is never dropped just because the panel
// still has focus. An id the page has never seen is a NEW node — mount it
// into the tree at its sorted position (a page with no `#tree` element,
// e.g. the d3 tree view's side pane, simply has nothing to mount into, and
// mountPanel no-ops).
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
// always stay first. Only `/legacy`'s outline carries a `#tree` outline
// element; elsewhere this is a deliberate no-op.
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
  // A payload-bearing sum's branch reveal: wire each sum's own radios to
  // re-evaluate on change, and set the initial state now. `querySelectorAll`
  // finds nested sums too (a variant payload can itself contain a sum); each
  // reveal is scoped with `:scope >` so a nested sum's radios never drive its
  // parent's toggle.
  root.querySelectorAll('[data-node="sum"]').forEach((sumEl) => {
    updateSumReveal(sumEl);
    sumEl.querySelectorAll(':scope > .enum input[data-kind="enum"]').forEach((r) => {
      if (r.__wiredReveal) return; r.__wiredReveal = true;
      r.addEventListener('change', () => updateSumReveal(sumEl));
    });
  });
}

// Show the `.variant-payload` whose `data-for` matches `sumEl`'s own checked
// radio value, hide every other one — the client-side replacement for the
// old data-derived `<style>` reveal. Every value compared here (`data-for`,
// a radio's `value`) is an ordinary attribute, already maud-escaped by
// construction; nothing here is interpreted as markup or CSS, so no
// model-authored constructor name or bind path can escape the text-only
// invariant through this path.
function updateSumReveal(sumEl) {
  const checked = sumEl.querySelector(':scope > .enum input[data-kind="enum"]:checked');
  const chosen = checked ? checked.value : null;
  sumEl.querySelectorAll(':scope > .variant-payload').forEach((p) => {
    p.classList.toggle('active', p.getAttribute('data-for') === chosen);
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
// data-kind: int/number -> number, bool -> boolean, enum/text -> string. A
// radio group shares one key; only the checked option contributes.
//
// int and number share one coercion path: an empty field submits `null`
// (the server's missing-key report, not a silent `NaN`), and anything that
// doesn't parse to a FINITE number (garbage text, or `Number("")` edge
// cases) submits `null` too rather than `NaN` — `JSON.stringify(NaN)` is the
// bare token `null` already, so refusing to send it as a NUMBER and instead
// sending it as the SAME null a missing field would send keeps the two
// indistinguishable to the server's validator, never a silently-accepted
// non-finite value. `number` additionally accepts a decimal point/exponent
// (`Number("3.5")` parses where `int`'s `<input step="1">` UI already
// discourages one) — the numeric leaf `Double` derives.
function collect(form) {
  const body = {};
  form.querySelectorAll('[data-bind]').forEach((f) => {
    const key = f.getAttribute('data-bind');
    const kind = f.getAttribute('data-kind');
    if (kind === 'bool') { body[key] = !!f.checked; return; }
    if (f.type === 'radio') { if (f.checked) body[key] = f.value; return; }
    if (kind === 'int' || kind === 'number') {
      const n = f.value.trim();
      if (n === '') { body[key] = null; return; }
      const parsed = Number(n);
      body[key] = Number.isFinite(parsed) ? parsed : null;
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

// Shared connection-indicator toggle — both views carry a `#conn` badge.
function setConn(ok) {
  const s = document.getElementById('conn');
  if (!s) return;
  s.className = 'conn ' + (ok ? 'ok' : 'down');
  s.textContent = ok ? 'live' : 'reconnecting';
}
"#;

/// `/legacy`'s own page-specific script: opens `/sse` and applies every
/// patch-elements frame straight into the outline via [`CORE_JS`]'s
/// `applyPatch`/`mountPanel`, then wires the initial DOM. The d3 tree view
/// ([`crate::tree`]) has its own page-specific script instead — refetching
/// `/api/tree` on every tick rather than mounting into a `#tree` outline —
/// but shares this exact [`CORE_JS`] for everything else.
pub const JS: &str = r#"
(function () {
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
        let doc = page(
            vec![("n1".to_string(), html! { div id="panel-n1" { "hi" } })],
            None,
        )
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
        let doc = page(
            vec![
                ("alpha".to_string(), html! { div id="panel-alpha" {} }),
                ("beta".to_string(), html! { div id="panel-beta" {} }),
            ],
            None,
        )
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
        let doc = page(
            vec![
                (
                    crate::DEFAULT_NODE_ID.to_string(),
                    html! { div id="panel-root" {} },
                ),
                ("root/1-x".to_string(), html! { div id="panel-root/1-x" {} }),
            ],
            None,
        )
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
        let doc = page(
            vec![
                ("root".to_string(), html! { div id="panel-root" {} }),
                ("root/1-x".to_string(), html! { div id="panel-root/1-x" {} }),
                (
                    "root/1-x/2-y".to_string(),
                    html! { div id="panel-root/1-x/2-y" {} },
                ),
            ],
            None,
        )
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

    /// When a run id is set, the masthead carries it as small text — so a
    /// pre/post-restart run is distinguishable in a stale tab.
    #[test]
    fn masthead_shows_run_id_when_set() {
        let doc = page(vec![], Some("run-2026-08-20-abc123")).into_string();
        assert!(doc.contains("class=\"run-id\""), "{doc}");
        assert!(doc.contains("run-2026-08-20-abc123"), "{doc}");
    }

    /// No run id set (`None`) renders no masthead run-id text at all — the
    /// same page every caller saw before this existed.
    #[test]
    fn masthead_omits_run_id_when_absent() {
        let doc = page(vec![], None).into_string();
        assert!(!doc.contains("class=\"run-id\""), "{doc}");
    }

    /// `collect()` coerces a `data-kind="number"` field exactly like
    /// `"int"` — the fix for the paper cut where only `int` was coerced and
    /// a `Double` field's value (rendered `data-kind="number"`) fell through
    /// to the plain-string branch, posting e.g. `"3.5"` where the server's
    /// `NumberShape` validator only accepts a JSON number
    /// (`tests/operator_gate.rs`'s
    /// `submit_rejects_number_field_as_a_string_but_accepts_a_json_number`
    /// proves the end-to-end consequence over real HTTP; no JS runtime lives
    /// in this test suite, so this pins the source fix directly).
    #[test]
    fn collect_coerces_both_int_and_number_kinds_to_a_json_number() {
        assert!(
            CORE_JS.contains("kind === 'int' || kind === 'number'"),
            "collect() must coerce BOTH int and number data-kinds: {CORE_JS}"
        );
    }

    /// High-4 regression: the payload-sum branch reveal is a client-side DOM
    /// toggle (`updateSumReveal`), never data-derived CSS text built from
    /// model-authored constructor names/bind paths.
    #[test]
    fn sum_reveal_is_a_dom_toggle_not_data_derived_css() {
        assert!(
            CORE_JS.contains("function updateSumReveal(sumEl)"),
            "{CORE_JS}"
        );
        assert!(
            CORE_JS.contains("p.classList.toggle('active', p.getAttribute('data-for') === chosen)"),
            "{CORE_JS}"
        );
    }

    /// Empty input and anything that doesn't parse to a FINITE number both
    /// submit `null` — never a raw unparsed string, and never `NaN` (which
    /// `JSON.stringify` would silently flatten to the bare token `null`
    /// anyway, indistinguishable from a deliberate missing value).
    #[test]
    fn collect_number_kind_empty_or_non_finite_input_submits_null() {
        assert!(
            CORE_JS.contains("Number.isFinite(parsed) ? parsed : null"),
            "{CORE_JS}"
        );
    }
}
