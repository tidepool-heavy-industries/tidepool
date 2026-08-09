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
- DO NOT reimplement GHC (Inanna, 2026-08-08). Custom `TypeError`s and a
  small routing family are leverage; an exhaustive "is this type supported"
  classifier is a second, worse type checker that goes stale against the
  real one. The classifying family (`FieldKind`) carries exactly two kinds
  of equation: types we implement SPECIALLY, and shapes whose failure we
  explain better than GHC can. Everything else falls through to the
  `Generic` case and GHC reports it. An unrecognized type reaching a
  standard GHC error is the CORRECT outcome, not a gap to close. This binds
  the checkpoint interpreter in step 7 too: its supported set differs (lists
  are legal there), so it gets its OWN small routing family — do not unify
  them into one classifier, and do not grow either to cover types purely for
  message quality.
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
  - **Inherited-red is established by A/B, never by argument.** A red test
    you did not cause is a normal thing to inherit and a fine thing to
    report — and it is also the most convenient available excuse, so it
    carries a proof obligation. In YOUR OWN worktree: `git stash`, run,
    unstash, run. Same command both legs.
    - Make both legs CACHE-CONSISTENT (clear `$PWD/.cache` before each, or
      confirm both actually recompile). A change that alters a source
      fingerprint invalidates the compile cache, so a naive A/B measures
      cold-vs-warm rather than your diff. This is the trap a dev falls into
      while honestly trying to comply.
    - "The failing test file is not in my diff" is INVALID for a change to
      a global surface — a prelude, a pragma list, an effect row, a shared
      wire type. Everything that compiles through it is downstream whether
      or not its own file appears.
    - Comparing two states that BOTH contain your changes rules out the
      other change, not yours.
    - Report per-red SIGNATURES plus an explicit pre-existence claim, not
      bare counts. Your own new tests are never covered by inherited-red.
    Measured in this wave: the two devs given this instruction produced
    sound baselines; the one who was not produced a plausible argument that
    was wrong.

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

   **The current codec has CONFIRMED silent-corruption paths** (external
   review, 2026-08-08). These are the concrete case for the
   loudness-as-a-test requirement — not a hypothetical risk to guard
   against, a list of live defects to fix and pin:
   - `Nothing`, `Just x` and unit all collapse to `null` (`render.rs:127`).
   - Three different constructor encodings emitted, while Haskell expects
     `tag` (~`render.rs:259`).
   - `Maybe` null-decode loses `Just ()` and nested `Just Nothing`.
   - `ToJSON ()` emits `null` while `FromJSON ()` accepts only `[]`.
   - Sentinel STRINGS instead of errors at depth limits.
   - Scientific components defaulting to zero.

   Golden-test matrix, derived from that list: nested `Maybe`, unit,
   non-finite numbers, mixed-constructor sums, recursion, unknown fields,
   depth overflow. Each case must produce either a correct round trip or a
   LOUD typed decode error naming the codec change. A silent misparse is
   the one unacceptable outcome, and "loud" is a test, not an intention.

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

## POST-REBASE GATE (obligations accrued mid-wave)

This lane forked BEFORE root's jit-chain-2 fold (ConTags fix + retry-path
consolidation), so it carries a known inherited red: a full
`tidepool-harness` shard on this base fails 29, and root's shard on the
fixed tip is 166/166 with none of them reproducing. Rebase happens ONCE,
after every dev folds — rebasing between folds strands the remaining
children's merge-bases. Four things must happen at that boundary:

1. **Full `tidepool-harness` shard re-run** as transfer proof, not quick
   tier alone. This is what CONFIRMS the 29 dissolved instead of assuming
   it. Anything surviving is a genuinely new finding.
2. **Re-run the derived-sum-vs-`SumShape`-literal `==` form.** dev-2 hit a
   case-trap (`runtime_strlen` bad pointer 0x1) comparing a DERIVED sum
   shape to a literal, and isolated it precisely: derived PRODUCT vs
   product literal is fine, literal-vs-literal sums are fine, `show` of a
   derived sum is fine, all 12 types render correctly individually. ONLY
   derived-sum vs sum-literal traps. Its shape tests assert exact
   RENDERINGS instead, so coverage is unchanged either way.

   **What this re-run can and cannot prove** (external review, 2026-08-08 —
   the original "probably ConTags" reading was wrong). The crash path is
   not `KnownSymbol`; it is a tag-as-address escape into `FfiStrlen`
   (`primop.rs:1976` accepts unvalidated raw SSA; ~`2642` unwraps a
   one-field constructor and loads an address payload without requiring a
   literal tag; `0x1` is consistent with an unboxed tag word). **ConTags
   does not touch that path.** Therefore:
   - Still traps → real, 2-line repro straight to root, as agreed.
   - PASSES → this proves the REPRO MOVED, not that the defect was fixed.
     Record it as informational. Do NOT claim discharge, and do not remove
     the exact-rendering assertions on the strength of it.
   The hardening fix (typed-address-only `unbox_addr`, `TAG_LIT` gate,
   negative tests) is routed to a separate codegen dev — not this lane.
3. **`15-generic-surface-wave.md` is dual-edited** — this lane appended the
   resolved-`render` decision; root updated the checkpoint bullet. Keep
   BOTH.
4. **`14-generic-derived-askuser-prd.md` is dual-edited** — this lane made
   the error-UX amendment (acceptance criterion, UX section, plugin note);
   Inanna finalized content on root's tip `f5d2ee55` (`choose`/`chooseMany`
   promoted from deferred escape hatch to the chosen sibling surface; the
   spike named a GO/NO-GO gate for this PRD *and* the typed-subagent PRD
   18; alternatives record updated). Disjoint sections, no overlap in
   intent. Keep BOTH.

Say in the rebase receipt which conflicts appeared and how they resolved —
"kept both" is only checkable if it is stated.

## DONE CRITERIA

- Spike verdict reported (either way) with receipts.
- On GO: a fresh resident session declares the PRD's example ADTs and runs
  `askUser @DeployRequest` bare; `chooseMany` selects among runtime values
  with real labels; the old builder is deleted; prompt framing advertises
  exactly the paragraph-sized contract; all PRD acceptance tests green.
- Runtime-context refactor and Harness.Prelude landed (these do NOT gate
  on the spike).
