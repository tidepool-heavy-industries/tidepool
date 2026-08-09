# Generic-surface wave — anchor

**Status:** approved direction (2026-08-08); wave spawns after the current
harness-lifecycle queue drains. Spike may start earlier (additive).
**Sources:** Codex wizard-DSL review (Inanna-relayed), gpt-5.6 amendments
from the PRD-dev window (file anchors verified against tree by root), and
[`14-generic-derived-askuser-prd.md`](14-generic-derived-askuser-prd.md).

## Decisions

- **`deriving (Generic)` is the author contract.** Authored harness types
  derive `Generic` plus ordinary domain classes they themselves use.
  `ToJSON`/`FromJSON` leave the authored surface; JSON remains an internal
  wire format.
- **Shared traversal substrate, separate interpreters.** One set of generic
  representation/metadata utilities; distinct interpreters per consumer —
  conceptually `GForm` (finite renderable shapes), `GTypeDoc` (answer
  synopsis; may describe lists), `GCheckpoint` (lists + recursion),
  `GFormDecode` (UI optional/sum semantics). Supported sets differ BY
  DESIGN; no single codec class whose supported-type set is the
  intersection. All invisible behind `deriving (Generic)`.
- **Runtime options are a first-wave primitive, not a future edge case.**
  `askUser @T` covers type-defined structure only; choices that exist as
  runtime values (the wizard's converging phase selects among accumulated
  `ideas :: [Text]`) need a value-taking verb:
  `choose :: [(Text, a)] -> M a`, `chooseMany :: [(Text, a)] -> M [a]`.
  This is the canonical Haskell spelling of value-defined alternatives, not
  a second form DSL. In scope for the wave; the dogfood harness already
  requires it.
- **Runtime context is the runtime's job** (Inanna-approved). Authored
  callback becomes `render :: State -> Text`; the driver composes the full
  system message: author render + prior compaction summary + loop metadata +
  capability/finalization instructions. `loopCount` leaves authored `State`.
  Fingerprint note: the checkpoint fingerprint is source-derived, so this is
  hygiene, not churn reduction — and the driver must persist the iteration
  count in the checkpoint ENVELOPE so restart behavior stays continuous.
- **Checkpoint codec replacement is separately gated; wire-compat is the
  DEFAULT, not a requirement** (Inanna, 2026-08-08: "json is fine as a
  default but we can just break compat if it's cleaner/easier"). Target the
  current encoding (records→objects, lists→arrays, `Maybe`→value/null,
  nullary constructors→strings, tagged sums) where it falls out naturally;
  where matching aeson's shape costs real complexity, break instead — the
  cost of a break is old checkpoints unreadable → fresh session start,
  which is cheap while dogfood is paused. A break must be LOUD (typed
  decode error naming the codec change, discard-and-restart path), never a
  silent misparse. Residual churn either way is source-fingerprint churn
  from editing deriving clauses. Sequenced AFTER forms (forms are
  additive; persistence is a replacement in the subsystem that produced
  both dogfood crashes). Routed: extends the generic-surface lane's queue
  post-swap (they hold the interpreter context).
- **One-file harness prerequisite is explicit:** effect vocabulary available
  in scope ≠ effects present in M's row. Today the answerer compile omits
  the `RunLLMTurn` GADT/helpers entirely (see `haskell/lib/Tidepool/
  Harness.hs` module haddock). The extract-wave spec must first make stable
  effect types/helpers nameable in every compile, with `Member` controlling
  executability. Then effect-polymorphic `loop` in one module works: it
  typechecks under an abstract row and fails only at instantiation against
  the answerer's narrower row. (Aligns with the 2026-08-07
  type-system-expresses-capability decision.)
- **Interim hole synopsis is honestly shallow.** `uiof.rs` captures field
  LABELS but not TYPES, so the near-term synopsis is
  `Contribution { addedIdeas, draftDelta, advance }` — constructor and
  selector names only. Three routes, chosen route first:
  1. ship the shallow synopsis now (kills most of the duplication);
  2. extend extract metadata with field types (only if the wave slips);
  3. the generic `GTypeDoc` supersedes both.
  Do not describe (1) as the eventual output.
- **finalize inference:** already in flight (`finalize-anchor` dev); the
  `(finalize @T value :: M T)` prompt coaching is deleted on its landing.
  No wave work item.

## Immediate items (land before the wave spawns)

- Shallow hole synopsis → harness-lifecycle wave-1.5 (hole-card-side Rust).
- Wizard prompt dedup: authored render/prompts express domain policy only
  ("consult the operator when direction or taste matters"); platform
  mechanics taught once by the runtime framing.
- Wizard answer types go semantic (`Progress = Stay | Advance`,
  `Maybe Text` over sentinel-empty) — typed `runLLMTurn` supports this
  TODAY; generic forms only matter when the OPERATOR is asked for the ADT.

## Wave scope (generic-surface TL)

1. Spike (GO/NO-GO): `deriving (Generic)` + `askUser @T` through the real
   extract/JIT — generic-rep dictionary elaboration under the JIT is the
   unproven part. Round-trip a nested sum/product; one selector-aware
   `TypeError`. Freeze the answer encoding. (PRD delivery step 1.)
2. Core algebra + separate interpreters; diagnostics (visited-type set,
   source-level `TypeError`s); wire + web renderer.
3. `askUser @T` swap + `choose`/`chooseMany`; then Form-builder migration
   and deletion per the PRD.
4. Parallel early devs: runtime-context refactor (`render :: State ->
   Text`); `Tidepool.Harness.Prelude` — which has TWO parts: the curated
   re-export module AND a harness compilation profile supplying the
   standard extension set via GHC flags. Open decision for the wave:
   remove the conflicting generic `render` from the unqualified Tidepool
   prelude instead of hiding it forever (a special prelude must not become
   a compatibility bucket).
5. After: prompt-coaching collapse (the 30-line answerer framing shrinks to
   the three-verb paragraph) — gated on generic forms landing.

## Out of this wave

- State checkpoint codec cutover (own gate, see above).
- One-file harness (extract wave; prerequisite recorded there).
- Lists/recursive types in operator forms (PRD v1 non-goals; lists are fine
  in `runLLMTurn` answer shapes and checkpoints — different interpreters).
