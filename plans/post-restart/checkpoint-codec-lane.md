# Spec: checkpoint-codec TL (successor to generic-surface)

Owns two items cut from the generic-surface queue when that lane's delta
grew (Inanna, 2026-08-08 — lanes that accumulate separable work run too
long): the **`GCheckpoint` cutover** and the **`Tidepool.Form` builder
deletion**. Spawns on the post-fold tip, so everything generic-surface built
is underneath rather than beside you.

Decisions live in `plans/self-iterating-harness/15-generic-surface-wave.md`
(anchor) and `14-generic-derived-askuser-prd.md` (the PRD). Cite them; do not
re-derive.

## SCOPE QUESTION — settle this before writing any code

**Do not build a `GCheckpoint` interpreter until someone confirms we need
one.** Inanna challenged the premise (2026-08-08) and the challenge looks
correct. Written up here rather than quietly acted on, because the anchor
still says "custom `GCheckpoint` interpreter" and this contradicts it.

`Tidepool.Aeson` already has `genericToJSON :: (Generic a, GToJSON (Rep a))
=> a -> Value` and `genericParseJSON :: (Generic a, GFromJSON (Rep a)) =>
Value -> Result a` (`Aeson/Value.hs:155`, `Aeson/FromJSON.hs:90`). They work
off `Generic` ALONE. So the author-contract goal — an author writes
`deriving (Generic)` and nothing else — is reachable today by having the
RUNTIME call those functions, instead of authors deriving `ToJSON`/
`FromJSON` themselves. That requires no new interpreter.

The PRD's rejection of `ToJSON`/`FromJSON` was scoped to FORMS, and its
reasons are form-specific: a form needs structural metadata to render a UI
and selector-aware compile errors to teach an author. Persistence needs
neither. It needs to write state down, read it back, and not corrupt it.
Carrying "separate interpreters per consumer" from forms to persistence
generalizes the argument past where it reaches.

The confirmed corruption list below points the same way: those are BUGS in
the serialization path we already have, largely Rust-side rendering. Fixing
them fixes every `ToJSON`/`FromJSON` user. Writing a second serializer beside
them leaves them broken for everyone else and leaves two things to keep
correct.

**Probable real scope, pending confirmation:** (1) fix the round-trip defects
listed under "Loudness is a test"; (2) move checkpoint persistence to call
`genericToJSON`/`genericParseJSON` so authored types need only
`deriving (Generic)`; (3) pin the golden matrix. That is a bug-fix and
wiring job, not a new codec — and if it turns out to be right, this lane
should be renamed, since "codec" is the framing that produced the
overreach.

## ANTI-PATTERNS (read first)

- **DO NOT reimplement GHC** (Inanna, 2026-08-08). Custom `TypeError`s and a
  small routing family are leverage; an exhaustive "is this type supported"
  classifier is a second, worse type checker that goes stale against the
  real one. A type you do not recognize falling through to a standard GHC
  error is the CORRECT outcome, not a gap to close.
- **DO NOT unify the interpreters.** `GCheckpoint`'s supported set DIFFERS
  from the operator-form interpreter's by design: lists and recursion are
  LEGAL in checkpoints and ILLEGAL in operator forms. Write a second small
  routing family over the shared traversal utilities. Do not converge the
  two to their intersection, and do not grow either to cover the other.
- **DO NOT route through `FromJSON`/`ToJSON`.** Rejected by decision record.
  JSON remains an internal wire format, not an authored contract.
- **DO NOT let a decode failure be silent.** See "Loudness is a test".
- Operational, copy VERBATIM into every dev spec:
  - Every GHC-heavy run goes through
    `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
    (absolute path). NEVER `exclusive` mode.
  - `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness test
    shard (persistent per-worktree, not mktemp). `.cache/` is gitignored.
  - Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs); never
    fable.
  - Never path-unscoped `pkill -f`; scope kills to PID or full worktree
    path. `pgrep -f` matches agent prompts — verify by /proc cwd first.
  - Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
    protected.
  - Grep/Read over LSP; do not start per-worktree rust-analyzer.
  - **Inherited-red is established by A/B, never by argument.** In YOUR OWN
    worktree: `git stash`, run, unstash, run — same command, both legs
    CACHE-CONSISTENT (a fingerprint-invalidating change otherwise measures
    cold-vs-warm cache rather than your diff). "The failing test file is
    not in my diff" is INVALID for a change to a global surface (prelude,
    pragma list, effect row, shared wire type). Two legs that BOTH contain
    your changes rule out the other change, not yours. Report per-red
    signatures plus an explicit pre-existence claim. Your own new tests are
    never covered by inherited-red.

## What generic-surface already established (do not rediscover)

The interpreter substrate is built and proven through the real extract/JIT.
Five things cost that lane real time to learn:

1. **`GHC.Generics` must be imported QUALIFIED** anywhere the Tidepool
   prelude is in scope. Bare `from`/`to` is an ambiguous occurrence against
   `Control.Lens.Iso.from` / `Control.Lens.Getter.to`.
2. **A recursion guard must be a `Bool` the interpreter DISPATCHES on, not
   a `Constraint` beside an extended path.** Instance heads match on the
   generic REPRESENTATION, not on the path, so an erroring path rides along
   as an opaque type and the next level is demanded anyway — GHC unrolls
   forever, 60s+ CPU, no error. See `Occurs`/`GNested` in
   `Tidepool.Form.Check` and `Tidepool.Form.GForm`.

   **This does NOT transfer to persistence, and an earlier draft of this
   spec said it did — that was wrong** (Inanna caught it). A form derives
   its shape from the TYPE with no value in hand, so a recursive type
   yields a genuinely infinite shape and must be rejected at compile time.
   Persistence walks a VALUE: recursion is ordinary, `Node Leaf Leaf` is
   finite, and the existing path serializes it today. Only an infinite
   VALUE diverges, and that is an ordinary Haskell infinite loop — not
   something a serializer should police. Do not port `Occurs` here, and do
   not invent a finiteness guard to replace it.
3. **A `TypeError` fires only where GHC must SOLVE it** — an instance
   context, discharged at instance selection. Written as a GIVEN (a
   binding's own signature context) it defers to call sites and reports
   there, or nowhere if the binding is never used.
4. **Metadata proxies are real `Proxy` constructors, never `undefined`.**
   `undefined` is JIT-safe but the tree-walking eval interpreter — the
   JIT's differential ORACLE — forces it, and a bottom there diverges the
   two engines. Vendored Aeson carries the same scar.
5. **Whether a type HAS an instance is not observable from a type family.**
   A stuck `Rep a` is indistinguishable from any unreduced family
   application. Carrying the field name inside the unsolved constraint (see
   `NeedsDerivingGeneric`) is the available move; prescriptive correction
   text for that case needs a typechecker plugin. Do not re-attempt it.

**One live trap that will bite this lane specifically.** Comparing a DERIVED
shape value to a hand-written literal of the same type case-trapped for the
form interpreter — a tag-as-address escape, isolated to derived-SUM vs
sum-literal only (derived products vs product literals are fine, literal vs
literal is fine, `show` of a derived value is fine). A checkpoint codec's
tests are FULL of "derive a value, compare to the expected literal", so
expect to meet it. The form lane worked around it by asserting exact
RENDERINGS instead, which pins every key, constructor and order and loses no
coverage. `strlen-hardening` has since landed, so the signature is now a
clean `ShapeTrapKind::AddrKind` poison+breadcrumb trap rather than a
segfault; a sibling `unbox_bytearray` gap of the same shape may still be
open. If you hit it, that is a real defect and it goes to root with a
minimal repro — not a thing to design around silently.

## GCheckpoint

The custom interpreter replaces the aeson-derived state encoding. Sequenced
AFTER forms because forms are additive and this is a REPLACEMENT — in the
subsystem that produced both dogfood crashes.

### Wire compatibility is the DEFAULT, not a requirement

(Inanna, relaxing the anchor as first written.) Target today's encoding —
records to objects, lists to arrays, `Maybe` to value/null, nullary
constructors to strings, tagged sums as today — so old payloads stay
readable. But where matching aeson's shape costs REAL complexity, break
instead. Breaking is cheap while dogfood is paused.

### Loudness is a test, not an intention

A break must surface as a TYPED decode error naming the codec change,
leading to discard-and-restart. **A silent misparse is the one unacceptable
outcome.** "Never silently misparses" is exactly the kind of property that
quietly stops being true, so it is pinned by tests rather than asserted in a
comment.

This is not hypothetical. The CURRENT codec has confirmed silent-corruption
paths (external review, 2026-08-08 — see root's
`plans/post-restart/codex-review-2026-08-08.md`, ledger item 8):

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

### Fingerprint churn

The checkpoint fingerprint is source-derived, so editing deriving clauses
churns it. That needs the restore/discard story, NOT a data-wire migration —
do not confuse the two.

## Form-builder deletion

PRD step 6 and its migration rules. The applicative builder (`Form`,
`enumField`, `intField`, `textField`, `boolField`) comes out of the
auto-imported and advertised surface, fixtures and docs move to ordinary
`Generic` ADTs, and positional `f<n>` field generation is deleted — positions
live only inside positional product nodes, never as a form-wide counter.

Check before deleting: the flat `FormSpec` path in
`tidepool-harness/src/selfharness/operator.rs` was deliberately kept alive
alongside the recursive one so the live `askUser` route never broke
mid-wave. Once `askUser @T` is the only caller, that flat path goes too —
confirm nothing else consumes it first.

Prompt framing is the acceptance test that matters: after deletion the
advertised contract is the PRD's one paragraph and one example. No
implementation classes, no wire values, no old field constructors anywhere
a model reads.

## VERIFY

- `cargo check --workspace` after every fold; quick tier
  (`cargo nextest run`) green.
- Real-extract acceptance via `scripts/ghc-slots.sh run -- ...` with
  `--ignore-default-filter`, `XDG_CACHE_HOME` exported.
- Receipts are per-binary COUNTS, never exit codes.
- **Verify through the TREE's extract** (`TIDEPOOL_EXTRACT` from a `cabal
  build`), not `tidepool-repl`. phase-b landed a wire break whose redeploy
  is deferred to dogfood resume, so the deployed extract is behind the tree.
  It fails loud rather than misparsing — recognize it rather than debug it.
