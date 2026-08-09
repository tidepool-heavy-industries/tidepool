# Spec: generic-surface TL

Owns the `deriving (Generic)` author-contract wave: `askUser @T`,
`choose`/`chooseMany`, the interpreter family, and the Form-builder
retirement. Decisions are in
`plans/self-iterating-harness/15-generic-surface-wave.md` (anchor) and
`14-generic-derived-askuser-prd.md` (the PRD) — cite them, do not
re-derive. Spawns when a current lane closes (three-lane cap, Inanna
2026-08-08).

## ANTI-PATTERNS (read first)

- DO NOT route schema production or answer decoding through
  `FromJSON`/`ToJSON`. The PRD rejects them by decision record.
- DO NOT build one codec class whose supported-type set is shared across
  consumers. Separate interpreters over shared traversal utilities
  (`GForm`/`GTypeDoc`/`GCheckpoint`/`GFormDecode` conceptually); lists are
  legal in checkpoints and answer synopses, ILLEGAL in operator forms v1.
- DO NOT emit partial or invented type information anywhere a model reads
  (no `field :: ?`, no guessed types). Degrade to less detail, never to
  wrong detail.
- DO NOT touch checkpoint persistence in this wave. `GCheckpoint` cutover
  is its own later gate (wire-compatible target; see anchor).
- DO NOT proceed past the spike without reporting the GO/NO-GO verdict to
  root. The spike is a gate, not a first step.
- DO NOT advertise implementation classes, wire values, or the old field
  constructors in any model-facing prompt text.
- Operational, copy VERBATIM into every dev spec:
  - Every GHC-heavy run goes through
    `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
    (absolute path). NEVER `exclusive` mode.
  - `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness
    test shard (persistent per-worktree, not mktemp).
  - Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs);
    never fable.
  - Never path-unscoped `pkill -f`; scope kills to PID or full worktree
    path. `pgrep -f` matches agent prompts — verify by /proc cwd first.
  - Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
    protected.
  - Grep/Read over LSP; do not start per-worktree rust-analyzer.

## READ FIRST

- `plans/self-iterating-harness/14-generic-derived-askuser-prd.md` — the
  product spec, including delivery sequence and acceptance criteria.
- `plans/self-iterating-harness/15-generic-surface-wave.md` — decisions +
  amendments (separate interpreters, choose/chooseMany, prelude two-parts).
- `tidepool-mcp/CLAUDE.md` — how to add an effect; `haskell/CLAUDE.md` —
  toolchain/extract rebuild + deploy.
- `haskell/lib/Tidepool/Form.hs` (the surface being replaced),
  `tidepool-harness/src/uiof.rs` (shape-degradation precedent),
  `tidepool-harness/src/ui.rs` + `tidepool-web/src/render.rs` (wire +
  render), `tidepool-harness/src/engine.rs` (framing text),
  `tidepool-harness/src/selfharness/driver.rs` (system-message assembly).

## STEPS

1. **Spike (GO/NO-GO gate).** PRD delivery step 1: prove
   `deriving (Generic)` + `askUser @T` through the REAL extract/JIT —
   generic-rep dictionary elaboration under our JIT is the unproven part.
   Round-trip one nested sum/product; prove one selector-aware
   `TypeError`; freeze the recursive answer encoding. Report the verdict
   with receipts BEFORE spawning implementation devs. NO-GO → stop, report
   what broke; the fallback conversation happens at root.
2. **Core algebra + interpreters** (PRD steps 2–3): `FormShape`/decode,
   primitive leaves, `Maybe`, `()`, visited-type set, source-level
   `TypeError` dispatch. Separate interpreter per consumer from day one.
3. **Wire + renderer** (PRD step 4): recursive products/sums in the shared
   Rust spec types and web renderer; exact-key tests.
4. **`askUser @T` swap + `choose`/`chooseMany`** (PRD step 5 + anchor):
   `choose :: [(Text, a)] -> M a`, `chooseMany :: [(Text, a)] -> M [a]` —
   the value-defined-alternatives channel; type-defined structure and
   value-defined options are DIFFERENT primitives, keep both surfaces
   one-paragraph small.
5. **Migration + deletion** (PRD step 6): fixtures/docs to Generic ADTs,
   builder de-advertised then deleted per the PRD's migration rules.
6. **Parallel early devs** (independent of the spike, may start
   immediately):
   a. Runtime-context refactor: authored `render :: State -> Text`; the
      driver composes render + compaction summary + loop metadata +
      capability instructions; iteration count moves to the checkpoint
      ENVELOPE (restart continuity), out of authored `State`.
   b. `Tidepool.Harness.Prelude` — BOTH parts: the curated re-export
      module AND a harness compilation profile supplying the standard
      extension set as GHC flags. Bring root the open decision: remove
      the conflicting generic `render` from the unqualified Tidepool
      prelude vs hide it (a special prelude must not become a
      compatibility bucket). RESOLVED — removal approved; see the anchor.
7. **`GCheckpoint` cutover** (added to this queue by root 2026-08-08,
   sequenced AFTER the `askUser` swap, since this lane holds the interpreter
   context). The custom checkpoint interpreter replaces the aeson-derived
   encoding. **One constraint RELAXED from the anchor as first written
   (Inanna):** wire-compatibility with today's encoding is the DEFAULT, not
   a requirement. Where matching aeson's shape costs real complexity, BREAK
   instead — breaking is cheap while dogfood is paused. A break must be
   LOUD: a typed decode error naming the codec change, leading to
   discard-and-restart. A silent misparse is the one unacceptable outcome.
   This is the subsystem that produced both dogfood crashes, and it is a
   REPLACEMENT rather than an addition — forms were sequenced ahead of it
   precisely because they are additive.

## VERIFY

- `cargo check --workspace` after every fold; quick tier
  (`cargo nextest run`) green.
- Spike + acceptance through the real extract:
  `scripts/ghc-slots.sh run -- ...` with `--ignore-default-filter -p
  tidepool-harness -E 'binary(<x>)'` (tier 2), XDG_CACHE_HOME exported.
- Receipts are per-binary COUNTS, never exit codes; `--no-fail-fast`
  where a known red exists.
- PRD acceptance-criteria sections are the DONE checklist for steps 2–5;
  the bounded malformed-submission re-prompt test stays green throughout.

## DONE CRITERIA

- Spike verdict reported (either way) with receipts.
- On GO: a fresh resident session declares the PRD's example ADTs and runs
  `askUser @DeployRequest` bare; `chooseMany` selects among runtime values
  with real labels; the old builder is deleted; prompt framing advertises
  exactly the paragraph-sized contract; all PRD acceptance tests green.
- Runtime-context refactor and Harness.Prelude landed (these do NOT gate
  on the spike).
