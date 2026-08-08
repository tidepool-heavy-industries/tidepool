# Addendum to finalize-template-pin — findings, decisions, and recovery

The first attempt at this task died mid-flight. Its work is preserved on
branch `root.harness-lifecycle.finalize-template-pin` at commit `6b2f5e9d`
("WIP: snapshot — two concurrent implementations interleaved, do not build").

Read `finalize-template-pin.md` first for the defect and the boundaries. This
file carries everything decided AFTER that spec was written. All of it was
settled in review; **none of it is open for re-litigation.**

## Step 1 is DONE — do not re-run the investigation

The cause is established, by minimal GHC repro, twice, by independent routes.

**The rule:** GHC's defaulting only fires when the ambiguous tyvar's constraint
set contains at least one class from GHC's own recognized standard set.
`ExtendedDefaultRules` relaxes standard rule 3 to *"at least one of the classes
Ci is numeric, **or is Show, Eq, or Ord**"* — a relaxation of the anchor
requirement, **never its removal**. It also widens the candidate TYPES. It does
not make a solitary user-defined class eligible to anchor defaulting on its own.

Our vendored `ToJSON` is an ordinary class with no superclass
(`haskell/lib/Tidepool/Aeson/Value.hs:147`), and `_r`'s only constraint is
`ToJSON a0`. No anchor ever rides with it, so defaulting is never attempted.

Evidence, all confirmed empirically with `ghc -fno-code`:

- `c undefined` for a user-defined `class C a` → FAILS.
- `c (fromInteger 5)` — `(Num a, C a)` on the same tyvar, nothing else changed
  → **COMPILES**. The anchor is the whole difference.
- `show undefined` (a lone standard class) → defaults fine.
- Adding `Show` to the constraint set makes it default correctly.

**Refuted hypotheses** — record them so nobody re-derives them:

- *Untouchability under an implication (`MonoLocalBinds` via `GADTs`, Eff row
  skolems).* Killed by the discriminator: identical `toJSON _r` shapes fail
  **identically** in a plain `IO`/`Identity` do-block and in a real
  freer-simple `Eff <row>` do-block. Plain `IO` is just as broken, so it is not
  an implication effect at all.
- *`ToJSON ()` missing.* Irrelevant — tested with a matching instance present,
  still failed. (`instance ToJSON ()` does exist, `Aeson/Value.hs:269`.)
- *Default-list composition.* Irrelevant — tested with `Int` in the list and a
  matching instance, still failed.
- *Monadic-bind vs generalized-binding.* Moot — it fails with zero binds, in a
  pure top-level expression.

## The mechanism — DECIDED: anchor-alongside

Supply the missing **anchor**, not a missing **type**.

For a turn compiled against a real (non-`NoAnswer`) `Finalize T` row ONLY,
render `_r` through a generated `__anchor :: Show a => a -> a; __anchor = id`
so the constraint set becomes `(ToJSON a0, Show a0)`.

Why this and not a hard pin (`_r :: T`):

- **It is additive.** A block ending in `finalize` leaves `_r` free, gains an
  anchor, and defaults. A block that is a bare non-bind `askUser form` is
  concrete `M Text`, needs no defaulting, and still compiles. A hard pin would
  unify `Text ~ T` and reject that turn — and a bare elicitation round (gather
  now, finalize next) is a real shape, not a hypothetical.
- **It needs only a boolean**, not `T`. So there is no threading of the answer
  type and **no parsing `T` back out of the rendered row string** — that
  round-trip was proposed and rejected: it discards structure you already have
  and breaks on the first qualified name or type application.
- **It never touches `finalize`'s own signature**, so it carries no
  `Translate.hs` dict-forwarding risk. The tyvar shape stays load-bearing and
  untouched, as the base spec requires.

Scope it to pinned rows. Do NOT add the anchor to the shared template — every
ordinary eval would then demand `Show` on its result type.

## Recovering the prior work — do this FIRST

`6b2f5e9d` contains a substantially complete `__anchor` implementation
(`eval_prep.rs`, `engine.rs`, `tidepool-harness/CLAUDE.md`, and a fuller
`finalize_type_pinning.rs`).

1. Recover the files from that commit into your worktree — e.g.
   `git checkout root.harness-lifecycle.finalize-template-pin -- <paths>`, or
   cherry-pick `6b2f5e9d`. Inspect before you trust: `git show --stat 6b2f5e9d`
   lists `CLAUDE.md`, `engine.rs`, `acceptance_finalize.rs`,
   `finalize_type_pinning.rs`, `eval_prep.rs`.
2. `__anchor` is the sole mechanism and the tree builds clean.

**Correction, verified after this file was first written:** an earlier draft
said `6b2f5e9d` did not build and carried a competing `FINALIZE_PINNED_HELPERS`
mechanism in `eval_prep.rs` to be deleted. Both were wrong. `git grep` finds
that name nowhere in `6b2f5e9d` or any working tree — the predecessor had
already finished reconciling to a single `__anchor` implementation before its
snapshot, and warned the tree might not build only because it had not
re-verified after its last edit. It builds, and check/fmt/clippy come back
clean but for the three pre-existing named warnings. Nothing needs deleting.

Do not preserve `6b2f5e9d` as a commit in your own history if a clean
recovery is simpler; it exists on the old branch as the durable record.

## Still required

- **Record which type the defaulting ACTUALLY picks** — `()` or `Int` — from a
  probe, not from reasoning about the docs. With `default (Int, Double, Text)`
  in scope alongside `ExtendedDefaultRules`, either is plausible; both have
  `ToJSON` and `Show` instances, so **the fix holds either way** and this gates
  nothing. It is required because an untested plausible detail handed to the
  next reader is how this thread already lost time twice. Observe it (a
  `Typeable`/`show` witness, or a constraint only one candidate satisfies) and
  put the answer in the receipt and in the comment at the fix site.
- **Bare non-bind `askUser form` coverage.** Mandatory. Under `__anchor` it
  should compile; prove it rather than assert it. It is the case that
  distinguishes this mechanism from the rejected hard pin.
- **The two-assertion test**, per the base spec as amended:
  1. drift-proofing — every shape the prompts prescribe compiles, DERIVED from
     the consts that generate them, not retyped;
  2. **bare `finalize @T value`, no annotation, compiles** — the fix's actual
     claim.
  Only (2) is the receipt: it must be mutation-closed, and (1) will not move
  under that mutation. Note the prompts currently prescribe the ANNOTATED
  stopgap shape `(finalize @T value :: M T)` (commit `d82cf099`), so (1) is
  trivially green today — that is expected, and it is not proof of anything.

## Your landing commit is the primary durable record

The earlier wrong cause was corrected FORWARD, not rewritten: `c03d7119`
supersedes `d82cf099`'s recorded hypothesis. So your commit message is now the
authoritative statement of the mechanism. State the anchor rule properly,
include the refuted implication hypothesis and the `IO`-fails-identically
evidence that killed it, so the next reader who re-derives it finds it already
answered.

## Verification

Unchanged from the base spec, and note the blast radius: you are changing a
template **every eval in the system shares**, so the `tidepool-runtime` and
`tidepool-mcp` GHC-heavy shards are required alongside `tidepool-harness`, not
optional. Several other GHC consumers are live on this box — expect slot waits
to stretch. That is elongation, not failure; do not trim coverage to go faster.
