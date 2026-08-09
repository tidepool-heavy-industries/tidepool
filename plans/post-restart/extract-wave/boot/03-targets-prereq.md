# Item 0 prerequisite — `--targets`: explicit multi-target emission

**Status: GO.** New work item, created 2026-08-09 after a premise correction
(wave TL's message, recorded at `a4642cba`; codex ledger item 10).

Runs in PARALLEL with wave 2 (`boot-lazy`): that lane is Rust-side session
lifecycle, this one is the Haskell writer side. No file overlap.
Wave 3 (`boot-onecompile`) cannot start until this lands.

## The correction that created this item

`plans/post-restart/one-compile-bootstrap.md:25` claimed render+loop
multi-target emission would come from "Phase B's multi-binder machinery", and
that claim was relayed into `00-spec.md` as "FOLDED and available to you".

**That machinery does not exist.** Phase B explicitly DEFERRED the
`writeWholeModuleClosed` work to a successor
(`one-spawn-turn-protocol-phase-b.md:99`). Verified in this tree:

- `writeWholeModuleClosed` takes a SINGLE `targetName`
  (`haskell/app/Main.hs:333–334`);
- the CLI has `--target <one name>` (`Main.hs:136`) and `--all-closed`
  (`Main.hs:138`). There is no `--targets`.

Phase B's actual multi-binder work is about tuple BINDERS on a session bind
turn (`Main.hs` ~671–736, "multi-binder: bound type is not a tuple") — a
different thing entirely from emitting several compile targets from one GHC
session. The gate opening was real; the machinery behind it was not.

## Goal

One `runPipeline` invocation, several explicitly-named targets, one merged
`meta.cbor`. This is what lets wave 3 compile `render` and `loop` in a single
extract spawn instead of two.

## The route (adopted, not optional)

`--all-closed` (`Main.hs` ~185–200) ALREADY proves one pipeline invocation can
translate several top-level binders. **Adapt that loop into a strict
explicit-target mode.** Do not invent multi-target extraction from scratch.

Two requirements distinguish it from `--all-closed`, and both are mandatory —
they are the entire reason this is a new mode rather than a reuse:

1. **It MUST fail if ANY requested target fails.** `--all-closed` catches
   errors from `translateModuleClosed` and SKIPS those bindings, by design,
   because it is a fixture sweep. That behaviour is exactly wrong here: a
   silently-missing `loop.cbor` would surface far away as a confusing runtime
   failure. Requested targets are a contract, not a best-effort sweep.
2. **It MUST preserve per-target asks and warnings.** `writeWholeModuleClosed`
   returns the `runLLMTurn`/`runLLMTurnFork` `{site, type}` pairs it wrote to
   `asks.json`; the harness's `classify_hole` consumes them. Two targets have
   DIFFERENT ask sites. Collapsing or merging them silently would misroute
   holes. Decide and document the on-disk shape (per-target `asks.json`, or one
   file keyed by target) and make the Rust reader match.

## Shape

- Add `--targets a,b` (or repeated `--target`; pick one and say why) alongside
  the existing flags. **Preserve the current single-target contract exactly** —
  `--target foo` → `foo.cbor`. Every other caller depends on it, and
  `processSessionFile` (`Main.hs` ~413, ~430) and the turn mode (~537) share
  `writeWholeModuleClosed`.
- Split `writeWholeModuleClosed` into (a) a per-target translate+collect step
  returning nodes + metadata pieces + ask sites, and (b) a single write step
  emitting one `<target>.cbor` per target plus ONE `meta.cbor` merged across
  all of them through `mergeMetaPreserving`.
- Keep `mergeMetaPreserving`'s LOUD collision behaviour. It keeps colliding
  (same-varId, different-qualified-name) entries distinct so the loader rejects
  them rather than one silently winning. Do not soften that to make a merge
  succeed.
- `meta.cbor` is shared, so its scalar fields need a defined multi-target
  meaning: `has_io` is the OR across targets, `var_names` the union. Both are
  diagnostic (`tidepool-repr/src/serial/read.rs` 82–88), so a union is honest —
  but write the rule down where the merge happens, in a comment.
- Rust side: a multi-target entry point in `tidepool-harness/src/compile.rs`
  (~104) returning one `CompiledTurn` per target over the shared merged table.
  Keep the existing single-target `compile_turn` as a thin wrapper so no other
  caller changes.

## Verification — the full wave gate (this touches extraction)

- hardened differential with floors intact:
  `TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh tidepool-codegen --run-ignored all -E 'test(haskell_suite_differential)'`.
  **`COMPARED_FLOOR` must not drop.**
- `corpus_report` — same shape, `-E 'test(corpus_report)'`
- extract-fidelity-test 26/26:
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- bash -c 'cd haskell && cabal test extract-fidelity-test'`
- harness acceptance:
  `scripts/battery-shard.sh tidepool-harness -E 'binary(/^acceptance_/)'`
- A test proving requirement 1: a two-target request where ONE target is
  bogus must FAIL extraction, not emit the good one and stay quiet. This is
  the requirement most likely to be quietly dropped, so pin it red/green.
- A test proving requirement 2: two targets with DIFFERENT `runLLMTurn` sites
  keep their sites distinct end-to-end.
- Extractor id-stability is PINNED (`session_table_qualified_identity` + two
  quick-tier assertions). A merge that changes a DataConId fires them — if it
  does, STOP and escalate; that is a design conversation, not a test to
  silence.

## Known fold point

`haskell/app/Main.hs` is shared with sub-TL `spawn-latency`, whose D1 reworks
`writeWholeModuleClosed`'s metadata merge (~line 348) — the same function this
splits. Deliberately not pre-partitioned (the realm-spike conflict experiment).
Minimal localized diff, no reorganizing of surrounding code, log anything
non-mechanical in `LEDGER.md` at fold.
