# WS3 design brief — the operator GUI aesthetic

**This is a design deliverable, not wiring.** The wiring already works
end-to-end (`--demo` serves the page, a submit round-trips a flat submission,
Continue advances). What is NOT yet done is the *aesthetic*, and that is the
locked deliverable — `09-askuser-form-gui.md` locked call 4:

> **Aesthetic bar: a data-viz poster that wins design awards.** Swiss /
> International Typographic Style — precise grid, generous whitespace,
> hairline rules, ONE restrained accent, semantic boxes, elegant type scale.
> No chrome, no gradients, no decorative shadows. Lines and boxes that carry
> meaning.

## What this page is

The ONLY human surface of a self-iterating LLM harness. An agent spawns a
typed form (`FormSpec` — a list of `enum`/`int`/`text`/`bool` fields) and
BLOCKS until the operator submits. Between loop iterations the operator
clicks Continue. Two interactions, one page, nothing else. Because it is so
small it has to be perfect: there is nowhere for a weak detail to hide.

Three states, all rendered by `render::panel`:
- `View::Form(spec)` — the fields + a Submit button. The main event.
- `View::Continue` — the between-loops gate: one Continue button.
- `View::Idle` — nothing pending. Should read as *composed*, not as an error
  or an empty state; the operator sees this while the model is thinking.

## Starting point

`shell::CSS` is a deliberate BASELINE, not a finished design: the right
constraints are written down (paper ground, one accent, hairlines, an 8px
unit) but the execution is flat and unconsidered. Treat it as a canvas that
already carries the correct constraints. **You may rewrite it entirely.**

## The bar — requirements, not suggestions

**Grid.** A single centered column with a real `max-width`. Every element
aligns to a spacing scale derived from ONE unit. No arbitrary pixel values
scattered through the sheet — if a number appears, it is a multiple of the
unit or a token.

**Whitespace.** Space is the primary compositional tool. The page must feel
calm and uncrowded even with four fields, and must not look empty with one.

**Hairlines.** 1px rules are the ONLY delimiters between semantic regions,
and each one carries meaning: field from field, masthead from body, actions
from form. No boxes-within-boxes, no nested panels.

**Forbidden.** Drop-shadows. Gradients. `border-radius` / rounded-card
chrome. Decorative icons or emoji. Any color that is not the single accent or
a greyscale value.

**Color.** ONE restrained accent on an off-white/paper ground; everything
else greyscale. The accent is a *scarce resource* — spend it on the single
most important thing on the page (the primary action, or the active
selection) and nowhere else. An accent used three times is used zero times.

**Type.** One family (a system stack — no external font; the CSP forbids it).
Two or three sizes, chosen deliberately, in a real ratio. Uppercase eyebrow
labels with generous tracking for field labels and section markers. Body copy
at a comfortable measure. Weight and letter-spacing do the work that size and
color would do in a lesser design.

**Fields.** Aligned to the grid, delimited by hairlines. An input should read
as a *ruled line to write on* or a precisely bordered box — not a rounded
form control. Style the native controls (radio, checkbox, number, text) so
they belong to the same design system; do not leave browser defaults sitting
inside your composition. Focus states must be visible and must not be a
browser-default blue glow.

**Feel.** Calm, legible, precise. It should look like a well-set page from a
Swiss design annual that happens to be interactive.

## Hard constraints (do not break these)

- **Self-contained.** Inline CSS only, inside `shell::CSS`. No CDN, no
  external font, no external stylesheet, no image URL. A strict CSP is
  assumed: any external host request fails.
- **Do NOT change `shell::JS`.** The vendored client is correct and is the
  partner to the wire contract below. Read it, style around it.
- **Preserve the wire contract.** Every input keeps `data-bind="<key>"` and
  `data-kind="enum|int|text|bool"`; the form keeps
  `data-on-submit="@post('/submit')"`; the continue button keeps
  `data-on-click="@post('/continue')"`; `panel()` keeps yielding a single
  `id="panel"` element (the SSE stream patches it by id). An enum radio
  submits its `tag` and displays its `label`. Restructure markup freely
  *within* those constraints.
- **`shell::page` keeps its signature** (`Markup -> Markup`) and keeps the
  `id="conn"` element the JS updates on SSE connect/disconnect.
- Keep the existing tests passing (update their markup assertions if you
  restructure, but do not weaken what they check).

## Review

Your parent TL will open `--demo` in a browser and review the aesthetic
hard. A merely-tidy page will be sent back. Iterate on the actual rendered
result — do not ship CSS you have not looked at.
