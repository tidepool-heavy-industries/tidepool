# Fable-era idea ledger (mined + verified)

Harvest of the ~2-3 day fable-driven era (2026-06-09..12), recovered after the
fallback to Opus 4.8. Sources: 4 miners (plans/git/memory, code-archaeology, a
felt-experience map, and a 32-file per-transcript fan-out workflow). Status
VERIFIED AGAINST HEAD on 2026-06-20 — the load-bearing step, because in-isolation
miners read *design* sessions and tag-as-unbuilt ideas that shipped later.

Full deduped ledger with verbatim quotes + transcript:line lives in the workflow
output (run wf_adb66328-4c3) and the per-agent transcripts. This file is the
curated, verified distillation. Complements (does not duplicate) HANDOFF.md's queue.

## Methodology caveat (keep in mind for ANY mined claim)
Miners faithfully report what a transcript SAID; several "specced-unbuilt" items
had shipped by a later session. ~3 of the fan-out's top-10 "net-new" were already
in HEAD. The proptest-campaign-report.md "open" items are MOSTLY already FIXED
(it's a hunt record, not a backlog). Verify before acting.

## ALREADY SHIPPED since voiced — DO NOT REBUILD (grep-confirmed)
- Render coercion class for `[fmt|]` holes; ghc-hs-meta `parseExp` frontend swap.
- PyF format specs incl. round-primop fixed-point float (`:.Nf`, no floatToDigits).
- `[sg|]`/`[uri|]` validator quoters; `[patch|]` + Diff verbs (applyDiff/genPatch/
  plan/apply/checkDiff, all-or-nothing, conflict-as-data, expr+pattern).
- closedBinds→reachable-closure meta walks (Wave 5, 909→172 entries); fail-loud
  varId-collision guard; lazy effect results; normalize.rs canonicalizer.
- try* failure isolation; flat-structures-over-Data.Map playbook (memory).

## OPEN — READY TO SPEC (Opus-actionable, verified absent)
- **Partial-output-on-failure**: drain the CapturedOutput buffer into the error
  result. "Today death is total amnesia, which makes big balls irrational." The
  Trace-effect's value without a new effect. [small]
- **Trace** (`Tick`), **Clock** (`now`/`timed`), **TxKV** (`Cas` for race-free
  saga checkpoints), **askChoice** (typed aperture menus) — effect-surface gaps.
- **Gotchas-as-tests**: executable Known-Limits registry; doc generated from CI so
  it can't rot. "When a 'fear' starts passing, the suite flags the doc as stale."
- **First benchmark suite**: "Nothing has ever been measured." Gates stage-3b
  zero-copy with data. (Our `lines` unpacks to a char list!)
- **base-re-exports for seeded Std**: "prefer base re-exports over hand-rolled list
  ops — structurally prevents the divergence-from-std bug class." Same own-the-
  source move as the text vendor.
- `[table|]`, `[cron|]` validators; per-eval timeout knob (cargoCheck→cargoTest).
- (Shamlet/[prompt|]/[ts|] explicitly DROPPED by the human / killed by dogfood.)

## OPEN — NEEDS DESIGN (the heavier bets)
- **Vendor Data.Text under our pipeline** — root-cause cure for the cross-module-
  unfolding class (HO-predicate landmine + #313 class); retires the takeWhileT/
  dropWhileT shadows. Aeson precedent. [IN FLIGHT: text-vendor agent, scout-first]
- **Type-level shape contracts** (`ValidatedConPtr<TAG>`, `RawHeapPtr`) — prevent
  the tag-mismatch garbage class at compile time, not by convention.
- **content-addressed Core** & **compile_to_callable** (both GO in future-plans.md);
  **block-level `try`** (codegen seam documented on respond_caught).
- Deprioritized by the human (recorded, not queued): Spawn effect, Haxl auto-
  batching Applicative, forkable apertures as speculative search.

## SHARPER ANGLES on known items
- HO-predicate corruption is the **higher-order predicate argument**, not eta-
  reduction: named predicates pass, operator-sections corrupt even saturated. The
  section's `'/'`/`Eq Char` evidence drops crossing a cross-module wrapper.
  (Best fixed BY the text vendor.)
- **B2 silent-garbage-on-bad-handler**: shape-mismatched resume → Int# math on a
  raw heap pointer → garbage; should be a clean trap. [IN FLIGHT: bug-cleanup]
- **Value::Drop closure-env residual**: iterative flattener skips Closure/JoinCont
  env vectors. [IN FLIGHT: recursion-sweep]

## PROCESS / POSITIONING (preserve fable's framing)
- "The MCP tool description should teach an arriving LLM the world" — `.tidepool/lib`
  location, `vocab` first, verb conventions, write→import→promote flow.
- "bash sessions evaporate; tidepool sessions compound" — promote-flow / defverb
  (property-test-then-register) / kvMemo flywheel.
- Capability-secure plugin runtime as commercial framing (effect-gated IO + host
  picks handlers; vs Lua/WASM). "The hylo boundary makes 'my tool calls' and 'a
  program I wrote' the same object."
- Testing: mutation testing (bullshit-detector for the suite), nightly eval-
  gauntlet (verb library as a continuous JIT integration test), AST-generator with
  twin render/eval, shape-dossier coverage ratchet, server-side CI gate.

## ANOMALIES (flagged, being resolved by bug-cleanup agent)
- heap-verifier: code-miner claimed a `heap_verify_enabled()` gate at host_fns.rs:
  ~352; grep for TIDEPOOL_HEAP_VERIFY came back absent. Contradiction.
- reachable-closure: merged Wave 5 but grep pattern missed the identifier; confirm
  actual names.

## IN FLIGHT (2026-06-20 wave, worktree Opus agents)
text-vendor (scout-first) · recursion-sweep (Value::Drop + spine family) ·
bug-cleanup (B2 guard + anomalies) · edit-surface (audit + propose line-range edit).
