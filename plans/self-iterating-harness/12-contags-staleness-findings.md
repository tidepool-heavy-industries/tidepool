# ConTags staleness and qualified-name identity — findings

Written during the `selfharness_compaction` tag-regression investigation. The
prune (`cb1b131d`) is not implicated here; these are separate defects the
investigation surfaced. Each is stated so an implementer does not have to
re-derive it.

## The symptom that led here

`YieldError::UnexpectedConTag` (`tidepool-codegen/src/yield_type.rs:43`) —
"Result was Con but con_tag was neither Val nor E". This is the freer-simple
classification of a RUN'S RESULT at the effect-machine boundary, not a case
dispatch and not an emission-time resolution failure. The heap-tag byte check
has already passed, so the object is a `Con` and its `con_tag` word is what does
not match.

## Finding 1 — the effect-result classifier is frozen at bootstrap

`JitEffectMachine.tags: Result<ConTags, &'static str>` (`jit_machine.rs:141`) is
assigned in exactly one place: `compile_inner`, `ConTags::from_table(table)`
(`jit_machine.rs:443`). That is the bootstrap turn only.

Every run entry reads `self.tags` — `jit_machine.rs` lines 635, 789, 914, 1551,
1696, 1851. The `table` argument threaded through `run_fragment` /
`run_with_entry` / `run_child_fragment` reaches handlers and the bridge; it is
never used to re-resolve `ConTags`.

`add_function` refreshes three other table-derived caches from each turn's
table — `lit_wrappers`, `json_con_ids`, `time_con_ids`, the latter two under an
explicit accumulate-never-clobber comment (`jit_machine.rs:1305-1317`). `tags`
is not among them.

So every post-bootstrap turn is classified by the bootstrap turn's `Val`/`E`
ids. That is correct exactly while re-resolving against the accumulated table
would give the same answer — see finding 2 for when it would not.

### Fix (mechanical)

Refresh `tags` in `add_function` alongside the other three, and settle the
semantics question the `Result` raises: the other two bundles use
upgrade-`None`-to-`Some`-never-clobber, but `tags` is a `Result` that can
legitimately go `Err(Missing…)` → `Ok`. Upgrading `Err` → `Ok` matches those
two. Overwriting `Ok` → `Err` would break a session whose later turn carries a
sparse table, so it must not.

Owner: `jit_machine.rs` (Cluster E).

### Finding 1b — the same omission has a second, deterministic effect

Because `tags` is a `Result` frozen at bootstrap, a bootstrap table missing any
of the five freer constructors leaves the session permanently
`JitError::MissingConTags`, even after a later turn's table supplies them. Not
the intermittent symptom under investigation — a clean, deterministic error —
but the same root omission, and fixed by the same refresh.

## Finding 2 — qualified-name identity is last-writer-wins under randomized order

Three facts compose into order-dependent constructor identity:

1. `DataConTable::by_qualified_name` is a `HashMap<String, DataConId>`
   (`tidepool-repr/src/datacon_table.rs:49`), and `insert` ends with a bare
   `by_qualified_name.insert(qn, id)`. Last writer wins, silently.
2. `insert_checked` (`datacon_table.rs:87`) guards only the `by_id` axis —
   same-id-different-identity, and same-id-disagreeing-tag/arity. Two DISTINCT
   ids sharing one qualified name pass straight through to `insert` with no
   check.
3. `DataConTable::iter()` is `self.by_id.values()` (`datacon_table.rs:333`) over
   a `std::collections::HashMap` with the default `RandomState` — iteration
   order is randomized per process. `PersistentSession::merge_table`
   (`tidepool-runtime/src/session/persistent.rs:348`) drives its inserts from
   exactly that.

`freer_names::resolve` consults `get_by_qualified_name` FIRST, so this map is
what decides which id `ConTags` treats as `Val`. If two ids ever share a freer
qualified name, which one wins varies per process — and nextest runs every test
in its own process.

### Fix (structural)

A `by_qualified_name` collision guard in `insert_checked`. Sequenced after the
in-flight `datacon_table.rs` work, and gated on the precondition check below:
making the collision a hard error would break any session where duplicates are
legitimate.

Owner: `tidepool-repr/src/datacon_table.rs`, after the current dev folds.

## The precondition, and where it is now asserted

Both findings are inert unless a real accumulated session table actually holds
two distinct `DataConId`s under one qualified name. `Val`/`E`/`Union`/`Leaf`/
`Node` come from version-locked packages, so their `stableVarId`s should be
identical in every turn's table.

Three permanent assertions pin it rather than leaving it to inspection:

| test | tier | what it pins |
|---|---|---|
| `tidepool-runtime/tests/session_table_qualified_identity.rs` | GHC-heavy | two real extracted turns, accumulated as `merge_table` does: one id per qualified name, and the bootstrap `ConTags` still classify the accumulated table |
| `datacon_never_used_as_value.rs::accumulated_corpora_keep_one_id_per_qualified_name` | quick | same invariant over the unioned corpora, always-on |
| `populated_session_second_fragment.rs::bootstrap_contags_still_classify_the_accumulated_table` | quick | the frozen classifier's precondition directly, with a positive control that re-mints `Val` to prove the assertion has teeth |

The real two-turn run settles the precondition as TRUE today: turn 1 contributed
124 constructors, turn 2 contributed 164, the accumulation is 164 — turn 1's set
is a strict subset with identical ids — and zero of the 164 qualified names
carry more than one id. Library constructor ids are stable across separate
extract invocations, so findings 1 and 2 are latent rather than live. They are
worth fixing because nothing states or enforces that extractor property, and
both failures would be silent.

## What the gate test constrains, by mutation

`populated_session_second_fragment` was mutation-checked on both axes it
claims, because a gate nobody has watched fail is not yet a gate.

- Minting `Val` in the bootstrap table under a different id than the fragment
  emits: all four tests RED, the child run failing as
  `Yield(UnexpectedConTag(10))` — the incident's own error variant. The
  run-and-classify half has teeth.
- Compiling the second fragment against the BOOTSTRAP table instead of the
  accumulated one — the mistake the file was written to catch: all four tests
  GREEN. Emission bakes each `DataConId` out of the frame and never consults
  the table to resolve a constructor reference. `add_function`'s `table`
  argument reaches `normalize`, `wrap_with_datacon_env`, `lit_wrappers` and the
  primop id bundles, none of which a synthetic `Con`/`Case` fragment exercises.

So the accumulated table is SETUP in that file, not an assertion. Catching a
wrong-table `add_function` needs a fragment that reaches one of those four
consumers — real extracted Core would, these hand-built trees do not. Recorded
in the file's own doc comment so the next reader does not mistake its green for
coverage of that axis.

One ambient hazard for whoever picks up the gate test: it drives
`run_child_fragment` with the parent's continuation stowed, and that path has a
known-incomplete `NurseryExhausted` guard (a one-shot gc_trigger-then-retry
landed at the Eager-response `value_to_heap` site and has since resurfaced
elsewhere on the same path under heavy allocation). A `NurseryExhausted` there
is that class, not a fault in this test.
