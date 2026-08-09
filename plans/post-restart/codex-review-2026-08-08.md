# Codex external review — 2026-08-08 (findings ledger + routing)

Four read-only audits by an external model family over the last ~day of
work, prompted from root's suspicion list. Worktree unmodified. One
verification limitation: Rust tests could not run in the review sandbox
(sccache `Operation not permitted`) — environment, not test failures.

Status legend: CONFIRMED = anchored in code; HYPOTHESIS = plausible,
reachability not established. Each item names its owning lane.

## 1. FfiStrlen tag-as-address escape — CONFIRMED latent; ConTags-independent
**Owner: unassigned (codegen hardening dev) · Affects: generic-surface gate**

The `==` case-trap's suspected cause is NOT KnownSymbol (generic impl uses
`datatypeName`/`conName`/`selName` packing ordinary String; no `symbolVal`
path). The real hole is lower: `tidepool-codegen/src/emit/primop.rs:1976`
accepts any raw SSA value without validating raw kind, and for heap values
recursively unwraps any one-field constructor and reads its literal payload
WITHOUT requiring a literal tag (~`primop.rs:2642`). An unboxed tag/word can
reach `FfiStrlen` as an address — `0x1` is consistent.

**Consequence for the generic-surface gate:** the ConTags fix does not touch
this path. A PASSING post-rebase `==` re-run proves the repro moved, not the
defect fixed. The gate outcome is informational either way; the hardening
item below is owed regardless.

Fix shape: `unbox_addr` accepts only explicitly typed address/raw-address
values; require `TAG_LIT` + expected literal representation before loading
an address payload; negative test feeding nullary and one-field non-string
constructors to `FfiStrlen`.

## 2. Compile cache cannot key GHC flags — CONFIRMED gap
**Owner: generic-surface (design input to dev-1's Harness.Prelude profile)**

Cache keys (see `tidepool-runtime/src/cache.rs:26`, compile call at
`tidepool-runtime/src/lib.rs:150`): rendered source, target binder, session
salt, include paths + recursive .hs/.hs-boot contents, extractor
fingerprint. NOT keyed: GHC flags/compilation profile, GHC/package-db
identity, working directory, cache-schema/JIT-ABI epoch.

Source pragmas are safe (part of rendered source). **A Harness.Prelude
compilation profile supplying extensions as command-line flags is exactly
the unsafe case** — neither the request nor the key can express flags, so
flag changes hit stale cache. Either the profile injects pragmas into
rendered source (cache-safe by construction) or the cache key gains a
flags/profile component first.

Also noted: the dedicated cache property suite genuinely distinguishes
hit/miss (not vacuously green); Haskell-verified and lazy-consumption
suites intentionally reuse warm caches and are weak evidence for cold
compilation or flag invalidation.

## 3. Realm registry: invariant is a test receipt, not a production check — CONFIRMED; plus one HYPOTHESIS
**Owner: realm-build**

Per-frame state is present as intended and the rejected-bottom path
correctly keeps the frame parked and rooted. But
`stowed_roots_count() == parked_count()` is asserted only in tests —
registry mutators don't check it. Add a `debug_assert` after every park,
rejected resume, successful removal, re-park, and realm drain.

HYPOTHESIS (reachability not established): parked response streams — their
registry is machine-global and cleared by each run guard; a continuation
holding an unforced streamed-response tail across an external realm park
could reference an ID cleared by another realm's run teardown. Wants a
focused realm test, not assumed a bug.

Also: JSON/time constructor metadata is machine-global; `add_function`
accumulates and rejects conflicting IDs — document as a global-ID invariant
and test with two realm tables.

## 4. Prefix check does not yet prove the seam's UnhandledEffect promise — CONFIRMED gap in the in-flight design
**Owner: realm-build lane B (step 3) — URGENT, signature changing now**

The realm-prefix branch verifies CALLER-SUPPLIED metadata while dispatch
remains through an opaque monomorphized `H`. If metadata says a tag lies
beyond the shared prefix but the concrete `H` has a handler at that
position, the request can silently reach the WRONG handler — a misroute,
the one outcome SEAM.md promises cannot happen. Prefix-checking metadata
alone does not establish "mismatch ⇒ clean UnhandledEffect".

Either require exact handled-prefix equality, or bind authenticated roster
metadata to the actual handler stack and select the correct per-frame
dispatcher dynamically.

## 5. GC walker design sound; falsification narrow — CONFIRMED both ways
**Owner: realm-build**

Not a missing-both-walkers defect: the continuation cell is an external
root slot (collection rewrites it, Cheney traverses the graph); the RBP
walker correctly doesn't see parked Box cells; other frame fields are
Rust-owned or already persistent-root-registered.

But the negative control (F3/F4, tag 221) exercises only captured
constructor chains. Nested/mid-effect continuations, streamed tails,
finalized closures, and binding parks are unfalsified. Add those shapes
before treating the suite as broad GC coverage.

## 6. Extension-list drift across four surfaces — CONFIRMED
**Owner: generic-surface (fold into Harness.Prelude work)**

Analogous extension lists already drift: production pragmas
(`tidepool-mcp/src/preamble.rs:27`), standalone session defaults
(`tidepool-runtime/src/session/render.rs:267`), binder parser profile
(`tidepool-runtime/src/session/binders.rs:21`), generated Effects module
(`tidepool-mcp/src/eval_prep.rs:116`). Only some are compared. Semantic
extensions (OverloadedStrings, scoped variables, defaulting/generalization,
deriving strategies, record-field resolution) can change meaning silently.

Fix: one canonical extension-set definition with explicitly tested per-
surface deltas; the Prelude re-export module and flag profile get a
set-equality test.

## 7. D1's "cheap visitor + hard subset assert" defense does not exist — CONFIRMED absent
**Owner: extract-wave (D1) — spec amended**

`collectUsedDataCons` is a second full unseeded translation
(`haskell/src/Tidepool/Translate.hs:1023`), not a syntax visitor; the
authoritative translation returns `tsUsedDCs` (`Translate.hs:443`); Main
SILENTLY UNIONS the two and proceeds (`haskell/app/Main.hs:348`). The
runLLMTurn/fork rewrite makes seeded and unseeded paths genuinely diverge,
so disagreement is possible. No missing constructor was found today, but
the promised fail-hard defense is absent — and silent under-collection is
the signature of the still-owed garbage-con_tag intermittent.

Required with the D1 fix: walk emitted FlatNode constructor/data-alt IDs
and hard-fail if output metadata omits any; independent reachable-Core
collector; a mutation test deleting one `recordDC` call must fail
extraction.

## 8. Checkpoint state codec: multiple silent-corruption paths — CONFIRMED
**Owner: generic-surface step 7 (GCheckpoint) — this is the pre-audit target list**

Envelope parser is appropriately loud; the state codec is not:

- Outbound generic Rust `value_to_json`, inbound authored Haskell
  `FromJSON` (`tidepool-harness/src/selfharness/state_cross.rs:56`) — not
  inverse by construction.
- `Nothing`, `Just x`, and unit all collapse to JSON null
  (`tidepool-runtime/src/render.rs:127`).
- Nullary constructors → strings, records → `_con`, positional → a third
  shape (~`render.rs:259`); Haskell sum decoding expects `tag` — formats
  not generally inverse.
- `Maybe` decode maps null → `Nothing`, losing `Just ()` / nested
  `Just Nothing`.
- Haskell `ToJSON ()` emits null; `FromJSON ()` accepts only `[]`.
- Depth limits / opaque runtime values become sentinel STRINGS, not errors.
- Malformed scientific components can default to zero.

Any of these can silently alter restored state or falsely implicate an
authored codec pair. GCheckpoint: one typed versioned codec both
directions, loud rejection of unsupported values; golden tests over nested
Maybe, unit, non-finite numbers, mixed constructor sums, recursion,
unknown fields, depth overflow.

## Routing summary

| # | Finding | Route |
|---|---------|-------|
| 1 | FfiStrlen escape | new codegen-hardening dev (unowned); gate-meaning note to generic-surface |
| 2 | Cache can't key flags | generic-surface, dev-1 design input NOW |
| 3 | Registry asserts + stream hypothesis | realm-build |
| 4 | Prefix check ≠ misroute-proof | realm-build lane B, urgent |
| 5 | Falsifier shapes | realm-build |
| 6 | Extension drift | generic-surface |
| 7 | D1 defense absent | extract-wave (spec amended in place) |
| 8 | Checkpoint codec | generic-surface step 7 target list |
