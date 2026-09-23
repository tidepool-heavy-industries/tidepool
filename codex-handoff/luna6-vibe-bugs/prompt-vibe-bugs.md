# Fix the bugs found in a headless proxy session

A headless run (`exomonad new`, `check`, `init --no-attach`, then cells through
`exomonad proxy`) found one engine bug and four workbench frictions. The
binary was built at 12:13 today, so first rebuild from HEAD and confirm each
item still reproduces; drop any that do not. Session notes and the exact cells
are in `/tmp/claude-1000/-home-inanna-dev-tidepool/465ece6e-1274-4abd-93b9-80ffdc838713/scratchpad/vibe-findings.md`
and `.../scratchpad/cells/`.

Rules: fix root causes in the owning module, not in callers; one commit per
item, by pathspec; add a focused regression test per item using the exact
failing cell; report what ran and what only compiled.

## 1. Any cell using `J..|` fails after the root's tools install (engine; highest priority)

    resident workbench execution failed: prepared engine: typed site
    12487394914945005776 is already installed by program ProgramId(1) with
    different evidence

Reproduces with a Choice packet and with a Score packet (both use `J..|`);
a Noul packet in the same session works. Fails on a fresh proxy workbench
too. Cells: scratchpad `cells/11.hs` (Choice) and `cells/13.hs` (Score):

    let rubric = J.level #low "low" (0 :: Int) J..| J.level #high "high" 1
    s <- J.ask (J.state (#task := ("t" :: Text))) (#q := J.score "How important is this?" rubric)
    fmap (\a -> a.q.expectation) s

### Diagnosis (done; verify, then fix)

VERIFIED:
- The site id has its high bit set, so it is a synthetic reply site
  (`syntheticSiteBit`, `bridge/haskell/src/Tidepool/PreparedSites.hs:316-326`),
  keyed only by a constructor's qualified name.
- Brute-forcing `syntheticSiteId` (GHC `fingerprintString`, checked against
  real GHC output) over every identifier the run compiled gives exactly one
  match: `Jev.Core.Schema.:|`, jev-dsl's
  `(:|) :: Alts f x -> Alts f rest -> Alts f (x :|: rest)` (the constructor
  behind `J..|`).
- `lowerVerbEvidence` (`bridge/haskell/src/Tidepool/ExecutionProjection.hs`
  ~:855-885) emits a synthetic `HostAnswer` row for EVERY interned constructor
  that `requestReplyIndex` accepts, deliberately without checking effect-row
  membership. `requestReplyIndex` (`PreparedSites.hs` ~:337-348) accepts
  `(:|)` because its result's last argument `x :|: rest` is lifted and has a
  nominal head; a polymorphic index is allowed.
- That index contains type variables. `TypePolicy.classifyType`
  (`bridge/haskell/src/Tidepool/TypePolicy.hs:141,147`) interns a type
  variable as `UnconstructibleG "polymorphic" (renderType ty)`, and the
  runtime's `type_nodes_equivalent` (`tidepool/runtime/src/session/prepared.rs`
  ~:1267-1277) compares Unconstructible nodes by their rendered text.
- The root's tools (template `Project/Shell.hs`, `Project/Lookup.hs`) use
  `J..|` in their Score rubrics, so ProgramId(1) owns the `(:|)` row first.

INFERRED (confirm by dumping both programs' site rows for this id):
- `renderType` uses `defaultSDocContext`, which does not suppress uniques.
  The kind variables GHC infers for `x` and `rest` (and `:|:`'s kind
  arguments) are system names, which render with their unique
  (`k_a1Bc`-style). Uniques differ between the root's compile and the cell's
  compile, so the rendered text differs and the rows compare unequal.

### Fix

1. Make type evidence deterministic: render with uniques suppressed
   (`defaultSDocContext { sdocSuppressUniques = True }`) in
   `TypePolicy.renderType`, and check `PreparedSites.renderType`, which feeds
   dynamic site identities, for the same issue. Better still, if variables
   can be rendered by position rather than by name, do that; two alpha-
   equivalent indices must produce equal evidence.
2. Decide, and state in `requestReplyIndex`'s comment, whether a constructor
   that is not an effect request (like `(:|)`) should get a synthetic row at
   all. Its index is unconstructible, so no host can ever answer it; the row
   is inert at best and, as here, a source of conflicts. The narrow rule is
   "only constructors of types that appear as effects", or "only indices
   whose evidence contains no Unconstructible polymorphic node". Pick one
   with the owner of `lowerVerbEvidence`; do not silently change which
   effect requests get rows.
3. Do not relax `sites_equivalent`.

Tests: an extractor-level check that two independent compiles of a module
using `(:|)` (or a minimal poly-kinded GADT with the same shape, in
`bridge/haskell/test-prepared-stg/site-fixtures/TypeEvidence.hs`) produce
byte-identical synthetic rows; and a runtime test that installs a program
whose cell asks a Score packet, then a program asking a Choice packet, on one
machine, both succeeding. Then rerun cells 10, 11 and 13 through the proxy.

## 2. Looking a name up from a cell

`lookup "Cmd.quiet"` in a cell is `Prelude.lookup` and displays
`<function>`. The hosted lookup is a tool; from a cell (and the proxy seat has
only cells) the path is `lookupRaw (LookupRequest ["Cmd.quiet"] False Nothing
3 [])`, a five-field request with no defaults.

Fix: give `LookupRequest` a defaulting constructor in `Tidepool.Lookup`, e.g.
`lookupRequest :: [Text] -> LookupRequest` with the defaults the hosted tool
uses, and say in the workbench skill and `exomonad/prompts/docs/workbench.md`
that a cell looks names up with `lookupRaw (lookupRequest [...])`. Do not
shadow or hide `Prelude.lookup`. Check that the hosted tool's defaults and the
new constructor come from one definition.

## 3. Result rendering

A value of type `Either e [(Text, Double, Double)]` rendered as:

    Right [(fib.py,
    0.96,
    0.17),
    (notes.md,
    0.27,
    0.75)]

Two defects: short compound values break one element per line, and `Text`
renders unquoted, so a string is indistinguishable from an identifier or a
number. Also `Cmd.stdout r` rendered `Right ([0, 1, ...]\n)` with the newline
breaking the constructor. Find the display renderer the workbench uses for
expression results (start at `tidepool/runtime/src/session/render.rs` and
`display`/`displayTree` in the Haskell library), make Text render quoted with
escapes like `show`, and keep a value on one line when it fits a width
budget, breaking only when it does not. Update the documentation tests that
assert rendered output; do not loosen them.

## 4. Empty name in a generation notice

A cell starting with `{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}`
prints `defined  at generation N` with an empty name. A pragma defines
nothing: it should produce no notice. Find where the cell splitter classifies
the pragma line (bridge/haskell cell splitter, `test-cell-splitter`) and where
the notice is rendered; fix the classification, not the message.

## 5. Bound command results echo their full observation

    r <- Cmd.run (Cmd.argv ["python3", "fib.py"])

prints the whole observation (session_id, terminal line, next hint, stdout)
and then `[bound r]`. With several commands in one cell this dominates the
output. Proposed: when a statement binds a command result, print one summary
line (job, exit status, stdout/stderr byte counts) and leave the full
observation reachable through the binding; unbound command statements keep
the full display. This is a behavior change to model-facing output, so
before implementing, confirm with the operator and check which skills and
prompts teach `Cmd.quiet` as the workaround; update them in the same change.

## Verification

Rebuild the extractor and `exomonad`, rerun the saved cells 06 to 12 through
`exomonad proxy` against a fresh `exomonad new` workspace, and paste the
before/after output for each item.
