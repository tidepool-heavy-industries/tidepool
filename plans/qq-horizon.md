# QQ Horizon — leveraging quasiquotation as the agent/effect interface

Status: brainstorm ratified by human 2026-06-11; sequencing below.
Context: post-EPS-unpoison, post-Lit-fix; fmt Phase 1 (Render + ghc-hs-meta holes)
in flight; PyF Phase 2 gated on the sibling-alt GADT fix (A′ vendor-mostly-intact
if fixed, B gutted-codegen fallback).

## Why QQs are disproportionately leveraged here

1. **Failure-latency arbitrage.** QQs run host-side in GHC at eval-compile time —
   no JIT constraints, errors are positioned compile errors. In an agent loop a
   compile error is ~free (fast, precise, no state mutated); a runtime effect
   failure is expensive (round-trip, diagnosis, partial state). Every format moved
   into a QQ moves its failures from the expensive class to the free class.
2. **Training-data channelling.** Models natively emit unified diffs, f-strings,
   JSON, regexes, tables, cron, SQL. A QQ wraps the type system around the dialect
   the model already speaks: dialect carries intent, types carry safety. Copy
   API/format shapes from training data (the aeson/PyF precedent), strip the guts.
3. **Desugar lands in the effect system.** A QQ can be a compile-time compiler
   from a familiar DSL to effect orchestration — not just a checked literal.

## Taxonomy (three species)

| Species | Work | JIT surface | Cost |
|---|---|---|---|
| Validators | compile-check, emit Text literal | ZERO | ~30 lines each over one shared `mkValidatorQQ` |
| Literals | compile-parse, emit structured data | constructors only | small |
| Orchestrators | compile-parse → effectful library verbs | verbs we control | medium |

## Keystone: `[patch|]` — edits as values (Orchestrator)

Replaces the Edit-tool paradigm (N calls, no atomicity, no pre-flight, collision-
prone `replace`) with patch-as-data:

```haskell
data Hunk      = Hunk { pre, del, ins, post :: [Text] }   -- context-anchored
data FilePatch = FilePatch { path :: Text, hunks :: [Hunk] }
type Patch     = [FilePatch]
```

- Compile-time checks: hunk line-count arithmetic, intra-file ordering/overlap,
  context presence, path sanity.
- Verbs (evolve .tidepool/lib/Patch.hs): `plan :: Patch -> M [Conflict]` (dry-run
  all hunks), `apply` (all-or-nothing: plan, commit only if clean), `invert`
  (diffs are invertible → rollback = apply . invert), `applyFuzzy` (explicit
  whitespace/offset policy), `genPatch old new` (Myers, pure — vendor `Diff`,
  BSD), `fromMatches` (sgFind sites → synthesized Patch).
- Unlocks: ATOMIC multi-file edits with pre-flight (structurally impossible in
  the tool-call paradigm); conflict-as-data + aperture (`ask` with conflict menu,
  resume with steering); generated patches from structural search; every eval
  returns its applied diff as a review artifact. Context anchoring retires the
  string-literal-collision gotcha that currently mandates whole-file writes.
- Pattern position (house specialty): destructure hunks/paths from diff text.
- Horizon: darcs-flavored commute/rebase (training data knows patch theory).

## Validator family (ship-anytime, zero JIT surface)

One combinator `mkValidatorQQ :: Text -> (Text -> Either Text ()) -> QuasiQuoter`;
then `[sg|]` (ast-grep metavar grammar — converts the documented "$$ARGS silently
no-match" class to compile errors; CHEAPEST REAL WIN), `[uri|]`, `[glob|]`,
`[cron|]`, `[re|]` (syntax-check only, emit Text for grepGlob/Rust side).

## Literals

- `[table|]` — markdown/CSV table → [[Text]]/rows; column-count checked; feeds
  Tab.* verbs and triage/survey batch ops. Pattern position: match rows.
- `[yaml|]` — Value literal; expression-only (no runtime parsing on JIT);
  needs a host-side YAML(-subset) parser vendored. Medium value — [j|] covers most.
- `[s|]`/here-doc raw strings — trivial throw-in.

## Orchestrators beyond patch

- ~~`[prompt|]` unified prompt+schema literal~~ — DEMOTED (2026-06-12 design
  discussion): once rendering is corrected to "prose → prompt channel, holes →
  schema channel" (no in-prompt field markers — platforms take schemas natively),
  the unified QQ reduces to [fmt|] (shipped) + the existing combinators + tuple
  sugar. Not worth a wave slot. The surviving residue: a SCHEMA literal for deep
  extractions where combinators get clumsy — `[ts|{...}|]` TypeScript-interface
  syntax (the most-trained schema dialect) producing Schema + parser for
  `obj … ?? prompt`. DEMAND-GATED: build only if dogfooding shows nested-obj
  extraction friction.
- Make-style recipe QQ over `run` + fsMetadata (Shake precedent) — e.g. the
  extract-rebuild→install→cache-clear dance as a checked recipe. Cute, not core.
- `[sql|]` — parked until a DB effect exists (human: "later, when we have/want db").
  Then: postgresql-simple surface + pattern-position row destructuring.

## Pattern-position as moat

Ecosystem QQs are expression-only; our [j|] pattern side generalizes via the
same ViewPatterns recipe to textual microformats with pure-Text (JIT-safe)
destructuring: `[uri|https://$host/$path|]`, `[kv|$key=$val|]`, table rows,
patch hunks. "Speak in formats, match in formats."

## Pruned (and why)

- Workflow/checklist DSL: Haskell `do` IS the orchestration language; revisit
  only if KV-resumable cross-eval DAGs prove needed.
- inline-c shape: tidepool already is the inline-language trick, inverted.
- Runtime `[re|]` matching: needs a pure JIT-side regex engine; validator
  version is free now, full version parked.
- Staged evals ([tidepool|] quoting sub-evals): quine territory; no eval-spawning
  effect exists; horizon note only.

## Sequencing

1. (in flight) fmt Phase 1: Render class + ghc-hs-meta holes.
2. PyF Phase 2 (A′ if sibling-alt GADT fix lands first, else B).
3. Validator family — leaf-sized, independent, can go anytime.
4. `[patch|]` v1: QQ + plan/apply/invert verbs + conflict-as-data. The joy project.
5. `[table|]`, `[prompt|]` design spike.
6. Pattern-position microformat family as demand appears.
