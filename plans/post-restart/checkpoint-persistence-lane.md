# Spec: checkpoint-persistence TL (successor to generic-surface)

Owns **checkpoint persistence round-trip correctness** and the
**`Tidepool.Form` builder deletion**, both cut from the generic-surface
queue. Spawns on the post-fold tip, so everything generic-surface built is
underneath rather than beside you.

Decisions live in `plans/self-iterating-harness/15-generic-surface-wave.md`
(anchor) and `14-generic-derived-askuser-prd.md` (the PRD). Cite them; do not
re-derive.

## Scope — bug-fix and wiring, NOT a new codec

Supersedes the anchor's custom `GCheckpoint` interpreter. Persistence needs
structural metadata for no UI and selector-aware errors for no author, so the
forms rejection of `ToJSON`/`FromJSON` does not extend to it. Three things,
and nothing else:

1. **Fix the confirmed round-trip defects in the EXISTING serialization
   path.** They are listed under "Loudness is a test" below. They bite every
   `ToJSON`/`FromJSON` user, not only checkpoints, and they are mostly
   Rust-side (`render.rs`).
2. **Move checkpoint persistence onto `genericToJSON`/`genericParseJSON`,**
   which already work off `Generic` ALONE (`Aeson/Value.hs:155`,
   `Aeson/FromJSON.hs:90`). This is what delivers the author contract — an
   authored harness type derives `Generic` and nothing else, because the
   RUNTIME calls those functions rather than the author deriving
   `ToJSON`/`FromJSON`.
3. **Pin the golden matrix** so the fixed behavior stays fixed.

Explicitly NOT in scope: a new interpreter, a custom codec, a port of the
forms recursion guard, or any invented finiteness guard.

## ANTI-PATTERNS (read first)

- **DO NOT build a second serializer.** We have one. It has bugs. Fix them
  where they are and every `ToJSON`/`FromJSON` user benefits; write a rival
  beside it and they stay broken for everyone else while we acquire a second
  thing to keep correct.
- **DO NOT port the forms recursion guard, or invent a finiteness guard.**
  Forms derive a shape from the TYPE with no value, so recursion yields an
  infinite shape and must be rejected at compile time. Persistence walks a
  VALUE: recursion is ordinary and `Node Leaf Leaf` is finite. Only an
  infinite VALUE diverges, which is an ordinary Haskell infinite loop.
- **DO NOT reimplement GHC** (Inanna, 2026-08-08). Custom `TypeError`s where
  we genuinely know something GHC does not are leverage; an exhaustive "is
  this type supported" classifier is a second, worse type checker that goes
  stale against the real one. An unrecognized type reaching a standard GHC
  error is the CORRECT outcome, not a gap to close.
- **DO NOT let a decode failure be silent.** See "Loudness is a test".
- Operational, copy VERBATIM into every dev spec:
  - Every GHC-heavy run goes through
    `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
    (absolute path). NEVER `exclusive` mode. Check whether root's
    box-wide throttle is still active when this lane spawns — under it the
    slot requirement widens to every heavy invocation, quick tier and
    `--workspace` check included.
  - `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness test
    shard (persistent per-worktree, not mktemp). `.cache/` is gitignored.
  - Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs); never
    fable.
  - Never path-unscoped `pkill -f`; scope kills to PID or full worktree
    path. `pgrep -f` matches agent prompts — verify by /proc cwd first.
  - Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
    protected.
  - Grep/Read over LSP; do not start per-worktree rust-analyzer.
  - **Pass `--no-fail-fast` EXPLICITLY; report COMPLETED vs CRATE TOTAL.**
    nextest defaults to fail-fast, so one red stops a run early while still
    emitting real pass lines with names and timings — a receipt
    indistinguishable from a complete one (observed: 198 of 877, 679 never
    run). Completed-vs-total is the only tell, so a pass count without its
    denominator is not a coverage claim. Applies until the worktree carries
    battery fix `7d57cea5`.
  - **One brokered leg per agent at a time; drain, don't kill.**
    `ghc-slots.sh detach -- <cmd>` exists to survive the QUEUE WAIT, not to
    parallelize. Before it, a queued run died at the ~380s kill and that
    death capped a non-adopter's footprint; detach removes the death, so
    several detached legs become durable simultaneous slot holds. Batch
    verification into ONE acquisition. And a killed GHC leg wastes the slot
    time already spent while freeing the slot no sooner than finishing —
    kill only known-void work (wrong branch, wrong command, already
    superseded), otherwise drain it and read the result.
  - **Watching a detached run: read the log RAW, and poll the PID.**
    Two traps, both hit in one session. (a) `ghc-slots.sh detach` echoes the
    whole command into its log, so a sentinel inside that command matches
    from line one — a `grep -q "===DONE===" log` watcher reports a running
    job as finished. Poll `kill -0 <pid>`. (b) A noise filter can remove the
    SIGNAL: `grep -v "^ghc-slots: acquired"` strips the one line reporting
    acquisition, leaving the earlier "all slots busy — blocking" line to be
    misread as current state. The log is two lines; read it whole. `fuser`
    on the slot files does NOT show flock holders here and will report every
    slot free while all six are held — scan `/proc/*/fd` for the slot paths
    instead, or just look at the process's own descendants.
  - **Name the INSTRUMENT beside any number.** A receipt states what
    produced a count, not only the count. An instrument that identifies its
    referent structurally (an in-code counter, a test that fails on
    divergence) beats one that pattern-matches something which merely looks
    right (`pgrep -f`, a grep over args) — three such external instruments
    were each wrong in a different direction on this box in one day.
    Per-binary nextest lines satisfy this already; anything else numeric
    needs it stated.
  - **Inherited-red is established by A/B, never by argument.** In YOUR OWN
    worktree: `git stash`, run, unstash, run — same command, both legs
    CACHE-CONSISTENT (a fingerprint-invalidating change otherwise measures
    cold-vs-warm cache rather than your diff; `cache_key_salted`
    fingerprints include-directory CONTENTS, so anything touching
    `haskell/lib` invalidates broadly). "The failing test file is not in my
    diff" is INVALID for a change to a global surface — prelude, pragma
    list, effect row, shared wire type. Two legs that BOTH contain your
    changes rule out the other change, not yours. Report per-red signatures
    plus an explicit pre-existence claim. Your own new tests are never
    covered by inherited-red.

## Loudness is a test, not an intention

A payload the current code cannot read correctly must surface as a TYPED
decode error, leading to discard-and-restart. **A silent misparse is the one
unacceptable outcome.** "Never silently misparses" is exactly the kind of
property that quietly stops being true, so it gets pinned by tests rather
than asserted in a comment.

This is not hypothetical. The following are CONFIRMED live defects (external
review, 2026-08-08 — root's `plans/post-restart/codex-review-2026-08-08.md`,
ledger item 8). They are the work, not the risk:

- `Nothing`, `Just x` and unit all collapse to `null` (`render.rs:127`).
- Three different constructor encodings emitted, while Haskell expects
  `tag` (~`render.rs:259`).
- `Maybe` null-decode loses `Just ()` and nested `Just Nothing`.
- `ToJSON ()` emits `null` while `FromJSON ()` accepts only `[]`.
- Sentinel STRINGS instead of errors at depth limits.
- Scientific components defaulting to zero.

### Golden matrix

Derived from that list — each case round-trips correctly OR produces a loud
typed error, and nothing in between: nested `Maybe`, unit, non-finite
numbers, mixed-constructor sums, recursion, unknown fields, depth overflow.

### Wire compatibility is the DEFAULT, not a requirement

(Inanna.) Keep old payloads readable where that is cheap. Where matching the
current shape costs REAL complexity, break instead — breaking is cheap while
dogfood is paused, provided the break is loud.

### Fingerprint churn

The checkpoint fingerprint is source-derived, so editing deriving clauses
churns it. That needs the restore/discard story, NOT a data-wire migration —
do not confuse the two.

## Established by generic-surface (do not rediscover)

1. **`GHC.Generics` must be imported QUALIFIED** anywhere the Tidepool
   prelude is in scope. Bare `from`/`to` is an ambiguous occurrence against
   `Control.Lens.Iso.from` / `Control.Lens.Getter.to`.
2. **A `TypeError` fires only where GHC must SOLVE it** — an instance
   context, discharged at instance selection. Written as a GIVEN (a
   binding's own signature context) it defers to call sites, or reports
   nowhere if the binding is never used.
3. **Metadata proxies are real `Proxy` constructors, never `undefined`.**
   `undefined` is JIT-safe but the tree-walking eval interpreter — the JIT's
   differential ORACLE — forces it, and a bottom there diverges the two
   engines.
4. **Whether a type HAS an instance is not observable from a type family.**
   A stuck `Rep a` is indistinguishable from any unreduced family
   application. Do not re-attempt prescriptive "add `deriving (Generic)`"
   text; it needs a typechecker plugin.

**Known trap — BUILDING a multi-variant sum literal.** A literal
`SumShape` with two or more `VariantShape`s dies on encode with
`[JIT] runtime_error kind=4 (TypeMetadata)` + `runtime_strlen: bad pointer
0x0` — no `==`, no derived value, no list result required. One-variant
literal sums, literal products (two and six fields), and the same
multi-variant sum DERIVED are all green, so building the literal is the
trigger, not comparing against it. Confirmed 2026-08-09 and escalated to
root; repro in a comment in `tidepool-runtime/tests/generic_form_wire.rs`.

**This lane will hit it immediately.** A golden matrix does not merely
compare against expected literals, it CONSTRUCTS them, and mixed-constructor
sums are one of its required cases. Assert from RUST on the returned `Value`
(see `generic_form_wire`) rather than comparing against a Haskell literal.
Escalate a fresh signature to root; do not design around it silently.

## Form-builder deletion

PRD step 6 and its migration rules. The applicative builder (`Form`,
`enumField`, `intField`, `textField`, `boolField`) comes out of the
auto-imported and advertised surface, fixtures and docs move to ordinary
`Generic` ADTs, and positional `f<n>` field generation is deleted — positions
live only inside positional product nodes, never as a form-wide counter.

generic-surface de-advertises the builder as part of the `askUser @T` swap
(a model-facing surface must not offer two form APIs), so what remains here
is migrating fixtures and deleting the implementation.

Check before deleting: the flat `FormSpec` path in
`tidepool-harness/src/selfharness/operator.rs` was deliberately kept alive
alongside the recursive one so the live `askUser` route never broke mid-wave.
Once `askUser @T` is the only caller, that flat path goes too — confirm
nothing else consumes it first.

## VERIFY

- `cargo check --workspace` after every fold; quick tier
  (`cargo nextest run`) green.
- Real-extract acceptance via `scripts/ghc-slots.sh run -- ...` with
  `--ignore-default-filter`, `XDG_CACHE_HOME` exported.
- Receipts are per-binary COUNTS, never exit codes.
- **Verify through the TREE's extract** (`TIDEPOOL_EXTRACT` from a `cabal
  build`), not `tidepool-repl`. phase-b landed a wire break whose redeploy is
  deferred to dogfood resume, so the deployed extract is behind the tree. It
  fails loud rather than misparsing — recognize it rather than debug it.
