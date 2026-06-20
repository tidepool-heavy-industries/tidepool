# Plan: Vendor Data.Text functions to kill the cross-module-unfolding bug class

Branch: `proptest-ghc-idioms.text-vendor` (worktree `/tmp/tidepool-textvendor`)
Status: **PHASE 2 — IMPLEMENTED.** Approved minimal scope (functions over external
type, module `Tidepool.Data.Text`). Awaiting final verify + commit.

## Phase 2 — what shipped
- `haskell/lib/Tidepool/Data/Text.hs`: drop-in `Tidepool.Data.Text` = `import
  Data.Text hiding (…)` + `module Data.Text` re-export, with HOME-Core bodies
  (text-2.1.2 verbatim) for the 15 predicate-class functions: takeWhile,
  takeWhileEnd, dropWhile, dropWhileEnd, dropAround, span, break, filter,
  partition, all, any, find, findIndex, split, groupBy. Predicate-applying
  HELPERS `span_`/`filter_`/`findAIndexOrEnd` are vendored LOCALLY (delegating a
  section predicate to an external helper is the red pattern). `all`/`any`/`find`/
  `findIndex` are direct iter loops (NOT stream fusion — fusion would delegate
  the predicate to external `S.all`/`S.any`/…). Text TYPE + instances stay
  EXTERNAL. No large Double/Integer literals (GMP-literal hazard N/A).
- Preamble repoint: `tidepool-mcp/src/lib.rs` — `effects_module_source` (the
  generated `Tidepool.Effects` module) AND `build_preamble` now emit `import
  qualified Tidepool.Data.Text as T`. One preamble-assertion test updated.
- `haskell/lib/Tidepool/Prelude.hs`: `import qualified Tidepool.Data.Text as T`;
  `takeWhileT`/`dropWhileT` RETIRED to thin aliases (`= T.takeWhile`/`= T.dropWhile`),
  String-detour bodies removed.
- CLAUDE.md Known-Limits entry updated (wrapped takeWhile/dropWhile = FIXED).
- Tests: `tidepool-runtime/tests/vendor_text_functions.rs` (16 cases, all 15 fns
  direct + wrapped under section predicates — all GREEN).
- Fixtures: NO regen needed — `Suite.hs` imports external `Data.Text` directly,
  not the preamble/vendored module/Prelude; its CBOR is independent.

## Phase 2 — verify status
- vendor_text_functions: 16/16 GREEN.
- repro_takewhilet_alias_pap (retirement gate): 14/14 GREEN with delegation form
  + String-detour removed — the red→green that justifies retirement.
- repro_takewhile_pap (direct external control): 14/14 GREEN.
- repro313, repro_qq_union (NONCE'd), haskell_suite, differential, clippy, fmt:
  running.

## TL;DR

The Phase-1 mechanism probe **empirically proved** the core hypothesis, and in
doing so revealed that we should vendor **functions, not the type**. A
home-module *real iter body* `takeWhile`/`dropWhile` is correct under the
operator-section + cross-module-wrapper landmine; the external delegation
(`= T.takeWhile`) is wrong even one home-module hop away. We do **not** need to
vendor the `Text` *type* (the bug lives in the function unfoldings, not the
representation), which avoids a large, risky migration of the Aeson `Value`,
the Prelude, the effect GADTs, and the Rust heap/render bridge.

## The probe (the proof)

Throwaway artifacts in this worktree (kept as living evidence; not for commit):
- `haskell/lib/Tidepool/TextProbe.hs` — `myTakeWhile`/`myDropWhile` = text-2.1.x
  verbatim iter-loop bodies, compiled as a HOME module over the EXTERNAL `Text`
  type + EXTERNAL `iter`; plus `delegTakeWhile = T.takeWhile` (external deleg).
- `haskell/lib/Tidepool/TextProbeWrap.hs` — a second home-module hop.
- `tidepool-runtime/tests/probe_text_vendor_mechanism.rs` — 6 cases.

Result (`cargo test -p tidepool-runtime --test probe_text_vendor_mechanism`,
`TIDEPOOL_GHC_LIBDIR=<with-packages>`):

```
home_body_takewhile_direct_pap   = ["hello","foo","noSlash"]   ✅
home_body_takewhile_wrapped_pap  = ["hello","foo","noSlash"]   ✅  (KEY)
home_body_takewhile_named_pap    = ["hello","foo","noSlash"]   ✅
home_body_dropwhile_wrapped_pap  = ["/world","/bar",""]        ✅
external_deleg_takewhile_direct  = ["hello/world",...]         ❌ (bug)
external_deleg_takewhile_wrapped = ["hello/world",...]         ❌ (bug)
```

Interpretation: the landmine is a property of the EXTERNAL-package interface
unfolding being routed through a home-module binding — NOT of the function body
nor the `Text` type. Translating the body in-session (home module) puts it on
the same Core path the EPS unpoison already fixed for direct use. Giving the
function a real home body is the fix; delegating to external is the bug.

## Static feasibility (all green)

- **Primops**: the JIT already implements the full ByteArray# surface text
  needs — `NewByteArray`, `IndexWord8Array`, `WriteWord8Array`, `CopyByteArray`,
  `CompareByteArrays`, `SetByteArray`, `SizeofByteArray`, … (`tidepool-repr/src/types.rs`).
- **Text FFI**: `mapFfiCall` (Translate.hs) intercepts by C-symbol infix —
  `_hs_text_measure_off`→`FfiTextMeasureOff`, `_hs_text_memchr`→`FfiTextMemchr`,
  `_hs_text_reverse`→`FfiTextReverse`, `strlen`→`FfiStrlen`. The vendored
  bodies keep the same `foreign import` C symbol names, so interception still
  applies. `_hs_text_memcmp2` (Array compare) **de-CPPs away** on GHC 9.12 to the
  `compareByteArrays#` primop branch (no FFI needed).
- **Representation**: the runtime already runs the real ByteArray-backed Text
  (`Value::ByteArray`, `runtime_text_measure_off` matches `text-2.1.2`
  semantics). It recognizes Text by `con_name == "Text"` + 3 fields
  (`tidepool-runtime/src/render.rs:345`, `:597`) — so keeping the EXTERNAL type
  means **zero** bridge changes.
- **Version**: boot/bundled text is **2.1.2** (the with-packages GHC ships
  `text-2.1.2-9a59`; the codegen FFI comments say 2.1.2). Vendor source must be
  text-2.1.2, NOT 2.1.3 (2.1.3 is wasm-only here).

## Recommended approach: vendor the FUNCTIONS over the EXTERNAL type

New module `Tidepool.Data.Text` (name TBD — see Decisions) that:
1. `import Data.Text hiding (takeWhile, dropWhile, span, break, filter, partition, find, …)`
   and `module Data.Text` re-export, so it's a drop-in for `qualified … as T`.
2. Re-defines the **predicate-taking** public functions with text-2.1.2's
   verbatim bodies (iter loops / `indices`), compiled as home-module Core:
   `takeWhile`, `dropWhile`, `takeWhileEnd`, `dropWhileEnd`, `span`, `break`,
   `filter`, `partition`, `find`, `dropAround`, `dropWhileEnd`, and the
   needle-taking family vulnerable to the same routing (`breakOn`, `breakOnEnd`,
   `splitOn`, `replace`, `isInfixOf` — validate each).
3. Keeps the EXTERNAL `Text` type, instances (`Eq`/`Ord`/`Show`/`IsString`/…),
   and the low-level internals (`Array`, `Unsafe.iter`, `Internal`, fusion,
   `Search`). NO instance redefinition (would be a duplicate-instance error),
   NO type vendor, NO Rust-bridge churn.

This is the smallest change that achieves the team-lead goals (kill the bug
class for the affected functions + retire the shadows), and it is **proven**.

### Alternative (fuller, higher risk) — for root to weigh
Vendor the entire strict-Text core (20 modules — see breakage map) including the
`Text` type + fusion + search as `Tidepool.Data.Text.*` home modules. Owns the
fusion/worker/wrapper unfoldings entirely. Costs: ~10 modules of de-CPP; a
vendored `Text` type collides with external `"Text"` in the DataConTable
(`get_by_name("Text")` ambiguity / loud `DataConCollision`) unless the entire
eval surface (Aeson `Value.String !Text`, Prelude, effect GADTs, Rust bridge)
migrates to it. No proven incremental bug-fix benefit over the recommended
approach (the probe shows the type is not implicated). **Not recommended unless
a fusion-function bug is later demonstrated through a wrapper.**

## Breakage map (strict-core, if the fuller approach is ever taken)

20-module strict-core closure (Encoding/Lazy/Foreign/bytestring cut). CPP counts:
`Array`(35), `Data.Text`(20), `Reverse`(14), `IsAscii`(13), `Show`(11),
`Fusion`/`Fusion.Size`(10), `Measure`(9), `Internal`/`Unsafe`(8), others ≤6;
`CaseMapping`(7787 LOC, 0 CPP), `Fusion.Common`(1213, 0), `Transformation`(340,
0), `Search`(103, 0), `Fusion.Types`(125, 0) copy verbatim. Cuts: `Binary`,
`Data`, `TH.Lift`, `PrintfArg` instances in `Data.Text.hs` (drop their
imports: `Data.Text.Encoding`, `Data.Text.Foreign`, `Data.Binary`,
`Text.Printf`, `Language.Haskell.TH`); SIMD validate; all Lazy/Encoding/IO.
De-CPP recipe: `cpp -traditional -P -undef -nostdinc` with the autogen
`cabal_macros.h` (in dist-newstyle) + GHC `MachDeps.h`, then drop the pragma
(precedent: the HsMeta vendor, `extract-session-constraints` memory).

## Shadow-retirement list (Prelude.hs)

Once `T.takeWhile`/`T.dropWhile` resolve to home bodies:
- `takeWhileT`, `dropWhileT` — RETIRE (the load-bearing `T.pack . go . T.unpack`
  manual shadows). They exist solely for this bug.
- Audit other predicate-routing shadows for the same retirement once their
  vendored counterparts validate.
- Keep FFI-gap shadows untouched (`round`, `showDouble`).

## Preamble repoint

`tidepool-mcp/src/lib.rs`: two sites emit `import qualified Data.Text as T`
(`:527` closed path, `:610` `build_preamble`) → repoint to the vendored module.
`haskell/lib/Tidepool/Prelude.hs:177` (`import qualified Data.Text as T`) →
repoint so the Prelude's own shadows that survive use the home bodies.

## Decisions for root
1. **Module name.** `Tidepool.Text` is taken (the camelToSnake utils, `TT.`).
   Options: `Tidepool.Data.Text` (recommended — clean mirror), `Tidepool.TextVendor`,
   or rename the utils module to free `Tidepool.Text`.
2. **Scope.** Recommended (functions-over-external-type, proven) vs fuller
   (full strict-core incl. type, higher risk).

## Fixture / test impact
- Repoint changes the preamble → MCP/runtime evals recompile against the new `T`.
- No `Suite.hs` fixture change expected (Suite imports `Data.Text` directly, not
  via preamble) UNLESS we change Suite to exercise vendored functions.
- New Phase-2 regression test: promote the probe to assert the home bodies green
  AND (as tripwire) keep the external-deleg contrast.
- Full verification gate: `haskell_suite`, differential, NONCE'd repros incl.
  `repro_takewhilet_alias_pap` (should now pass against the RETIRED shadow path),
  `repro_takewhile_pap`, `repro313`, `repro_qq_union`; clippy; fmt.

## Phase-2 sequencing (gated)
1. Build the vendored module; **prove** all target functions green under the
   wrapper+section stress (extend the probe). [no irreversible change yet]
2. Repoint the preamble + Prelude import.
3. Retire the proven-redundant shadows.
4. Regen fixtures if needed; full verification.
5. Commit (`Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`).
