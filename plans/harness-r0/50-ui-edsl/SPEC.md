# Spec: `Ui` eDSL + Datastar renderer + D1 tree skeleton

Haskell programs describe UI as DATA (`haskell/lib/Tidepool/Ui.hs`); the
web layer renders it. Haskell never sees Datastar/HTML — the eDSL is
purpose-specific and the translation lives entirely in `tidepool-web`.
R0 ships the ADT + renderer + the degenerate case (hole cards + tree
view); the full Dialog effect (B1–B5) is R1 and must not creep in.

FINALIZED AT SPAWN: fable designs the ADT below into contracts.md first;
sonnet leaves implement against it.

## ANTI-PATTERNS

- DO NOT expose widget/HTML/Datastar vocabulary in Haskell — the ADT
  speaks elicitation semantics (choice/text/prose/…), the renderer speaks
  Datastar.
- DO NOT build the Dialog EFFECT in R0 — no `Dialog` constructor in the
  stack, no `uiOf`, no `[form|]`. Only the ADT + rendering, exercised by
  the harness's own hole cards.
- DO NOT omit the open-prose path from any elicitation — every choice is
  an open sum (B2 is a law of the ADT, encoded in the type, not a
  renderer convention).
- DO NOT put UI state in Rust components — the renderer is stateless per
  render; state lives in the session tree + event log; updates arrive by
  SSE patch (Datastar fragments), never client-side state sync.
- Tree view: DO NOT assume small trees — subtrees collapse by default;
  rendering must stay usable at 10^3–10^4 nodes (virtualize or paginate;
  the PRD permits a bundler-free Preact island if Datastar attributes
  aren't enough — escape hatch, not default).

## ADT sketch (fable finalizes; R0 subset)

```haskell
-- Tidepool/Ui.hs — data only, no effects
data Ui
  = Card   { title :: Text, body :: [Ui] }
  | Prose  Text                      -- markdown
  | Code   Text Text                 -- language, source (type sigs, decls)
  | Choice { prompt :: Text, options :: [(Text, Text)] }  -- key, label; open-prose path implicit
  | TextIn { prompt :: Text, multiline :: Bool }
  | Badge  Text BadgeKind            -- effect-row / fan / price chips
```

Wire: `Ui → Value` via the existing aeson surface (the harness renders
holes by CONSTRUCTING `Ui` values Rust-side too — same JSON shape, one
Rust mirror type in contracts.md). R1 grows `Ui` into the monadic
sequencing + products story; R0 keeps it first-order.

## Leaves

### E1 (sonnet) — `Tidepool/Ui.hs` + JSON codec + stdlib registration
Per `haskell/CLAUDE.md` "Adding new Prelude functions": module under
`haskell/lib/Tidepool/`, `works_*` probe in
`tidepool-runtime/tests/jit_surface.rs`, ToJSON via the existing aeson
vendored surface.

### E2a (sonnet) — Rust renderer `Ui → Datastar fragment` (maud)
Table-driven per constructor; property: every `Choice` renders the prose
escape; snapshot tests on rendered HTML.

### E2b (sonnet) — D1 tree view skeleton
Server-rendered tree over the protocol's snapshot + SSE stream (segment
30 C4): per node — state glyph (thunk/running/suspended/waiting-on-
operator/done/cancelled), badges, teaser, force + answer controls in
place. <2s update via SSE patch; no manual refresh; collapse-by-default.

## VERIFY

- `jit_surface.rs` probe green (Ui values construct + serialize on the
  JIT); battery green.
- Renderer snapshot tests; a live smoke: run the harness binary, publish
  a synthetic hole, see the card + answer flow in a browser over
  loopback, tree updates without refresh.
