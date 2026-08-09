# Dev spec: hole-card type synopsis (wave 1.5)

Make the hole card render the answer type's shape from the compiled artifact,
so harness authors and the answerer framing stop pasting type shapes by hand.

```
Contribution { addedIdeas, draftDelta, advance }
```

**Sequenced AFTER `finalize-anchor` folds** — this lands in `engine.rs`, which
that dev owns until then.

## Decisions already made — cite, do not re-derive

This is the **interim leg** of a larger approved direction: a generic-derived
surface (`deriving (Generic)` as the author contract, `askUser @T`,
`choose`/`chooseMany`), where the full typed declaration eventually comes from
a `GTypeDoc` generic interpreter that SUPERSEDES this synopsis.

Both are committed on root's branch at `e6852a45`:
`plans/self-iterating-harness/15-generic-surface-wave.md` (anchor, decisions)
and `14-generic-derived-askuser-prd.md` (the askUser PRD). Read them with
`git show e6852a45:<path>` — do not merge or rebase onto that branch.

**Do NOT build `GTypeDoc` or any part of the generic interpreter.** This item
is deliberately the cheap interim: names only, from the table we already have.

## Honestly shallow — this is a constraint, not a limitation to work around

`uiof.rs`'s module doc states it plainly: the `DataConTable` **captures field
LABELS but not field TYPES**. So the synopsis renders constructor and selector
NAMES and nothing else.

Do not infer, guess, or annotate types. A synopsis that renders
`Contribution { addedIdeas :: ? , … }` or invents plausible types is worse than
no synopsis — the model will believe it. Names are genuinely useful on their
own (they are what an author currently hand-pastes); pretending to more is the
failure mode.

## Reuse the precedent, do not reinvent it

`uiof.rs` is the in-tree precedent for reading the table this way (`uiOf`
derives mechanical operator forms from a compiled `DataConTable`). Use its
accessor patterns — `DataConTable::constructors_of_type`, its `dataConTag`
ordering, its field-label access. If you find yourself writing new table
traversal, look there first.

## Degrade the way `uiOf` already learned to

`uiOf` returns `None` for: a multi-constructor type WITH fields, a single
constructor with no captured field labels (positional, not a record), and a
type name the table has no constructors for. Those cases are not
representable from labels alone.

The synopsis must degrade the same way: **render the bare type name and
nothing else.** Never emit a partial shape that implies more than the table
knows. Getting this wrong is the only way this item can do harm.

Supported, mirroring `uiOf`:

- all constructors have zero fields (a nullary sum) → the constructor names,
  in `dataConTag` order — e.g. `Advance | Hold | Abort`
- exactly one constructor carrying field labels (a record) → the selector
  names — e.g. `Contribution { addedIdeas, draftDelta, advance }`
- anything else → the type name alone

## Also in scope — relax the finalize prescription back to the bare shape

`answerer_hole_card` currently prescribes:

> Answer by evaluating `(finalize @{ty} value :: M {ty})` — annotate the WHOLE
> expression with `:: M {ty}` …

That whole-expression annotation is a **stopgap**, added when bare
`finalize @T value` did not compile (the ambiguous-`a0` defect). The
`__anchor` fix has since landed in this lane, and
`finalize_type_pinning::bare_finalize_with_no_annotation_compiles_when_pinned`
proves the bare shape now compiles for a pinned `Finalize T` row — which is
exactly the row an answerer turn uses.

So **relax it back to bare `finalize @T value`**, and drop the
"annotate the WHOLE expression" instruction with it.

Do this in the SAME pass as the synopsis, not as a separate change: both edit
the same hole card, both are read by the same consumer (the model), and the
prescription and the type synopsis should end up as one coherent hole-card
voice rather than two revisions layered on each other.

Three constraints:

- **Relaxation, not prohibition.** The annotated form still compiles and stays
  valid. Do not add machinery to reject it, and do not tell the model it is
  wrong — it is merely no longer necessary.
- **`hole_card` (~454) is NOT affected.** It prescribes `resume`, not
  `finalize`, and was never subject to this defect. Leave its text alone.
- The drift-proofing test
  `prompts_prescribed_hole_card_shape_compiles_when_pinned` DERIVES its example
  from this source rather than retyping it, so it follows your new text
  automatically and must stay green. If it goes red, the text you wrote does
  not compile — that is the test doing its job, not a test to update.

## Where

`engine.rs`'s `hole_card(prompt, ty)` (~454) and `answerer_hole_card(prompt,
ty, imports)` (~480). Both render a hole card as a user-turn message; both
already receive the answer type. They need the `DataConTable` too — thread it
from the compile artifact the caller already holds rather than recompiling or
re-deriving it.

Check `driver.rs`'s call site (~1038, `engine::answerer_hole_card(prompt, ty,
self.answerer_imports())`) for what the driver has available at that point. If
the table is not reachable there without restructuring, STOP and report rather
than plumbing a new parameter through several layers — the threading cost is
the thing that decides whether this item is cheap.

## Verify

- A record type renders its selector names.
- A nullary sum renders its constructor names in `dataConTag` order.
- **An unsupported shape (multi-constructor with fields, positional
  constructor, unknown type name) renders the BARE TYPE NAME and no partial
  shape.** This is the assertion that matters most — mutation-close it.
- The rendered synopsis is derived from the table, not from a hardcoded string:
  a test that changes a type's field names and sees the synopsis follow.

Standard tiers: `cargo check --workspace --all-targets`, `cargo fmt --all --
--check`, `cargo clippy --workspace` (three pre-existing warnings —
tidepool-codegen `large_enum_variant`, `engine.rs` `TurnOutcome`
`large_enum_variant`, `selfharness_compaction_fixes` `type_complexity` — are
not yours). Quick tier with the tests-RUN count. GHC-heavy:
`acceptance_finalize`, `finalize_type_pinning`, `acceptance_selfharness`,
`golden_path`.

## Standing environment rules

```
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
export XDG_CACHE_HOME="$PWD/.cache"
```

The extract binary is SHARED and READ-ONLY — never rebuild it, never touch
`haskell/`. `XDG_CACHE_HOME` is mandatory before any test run and must be a
persistent per-worktree dir, not `mktemp -d`; verify it empirically (an
isolated cache appears in your worktree, `~/.cache/tidepool/selfharness/`
mtimes unchanged).

- Every GHC-heavy run through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>` (absolute
  path). NEVER `exclusive` mode. Do not override `.config/nextest.toml`'s
  default-deny `ghc-heavy` group.
- Shard to ONE test per invocation; run foreground with a long timeout; gate
  on tests-RUN counts, never exit codes (a run killed at the ~380s boundary is
  indistinguishable from a failure by exit status — a short count means
  RE-RUN).
- No LSP / rust-analyzer — `grep` and `Read` only.
- Scope kills to your own PID or worktree path; never a bare `pkill -f`.
- Never `git add -A`; never force-push; repo-root `tmp/` is protected; commit
  with `--no-verify`. Commit at every checkpoint.
- A flaky test never lands.

## Done criteria

- Hole cards render a names-only synopsis for the two supported shapes, from
  the `DataConTable`.
- Unsupported shapes render the bare type name — mutation-closed.
- No type information invented anywhere; no `GTypeDoc` work.
- check / fmt / clippy clean; tiers reported with tests-RUN counts.
