# PRD 20 S1-L5 — resume: the read half of git-plus-journal persistence

The write half landed already: `record kind key payload` (`Tidepool.Journal`,
`tidepool-handlers/src/handlers/journal.rs`) appends one flushed JSON line per
completed step, and `harness-dogfooding/dev-tree/Harness.hs` calls it at every
split, outcome, replan, rebase, and escalation. Nothing reads it back.

This lane is the read half: **fold the journal at boot, inject the folded map
into the authored loop, and let a resumed run do only the delta** — plus
adopt-and-verify for commits a crash left orphaned in a retained worktree.

Realizes PRD 20 "Persistence and resume" bullets 3 ("Resume folds the
journal"), 4 ("Adopt and verify, never redo blind"), 5 ("Retained worktrees
rebind, not recreate"), and closes its open sub-questions (i)/(ii)/(iii).

---

## What is locked before this lane starts

Repeated here because every decision below is downstream of them:

- **`record` is WRITE-ONLY on the authored surface** (`Tidepool.Journal`'s
  module doc). Reading, folding, and injecting is the DRIVER's job. Nothing in
  this lane adds a read verb to the `Journal` effect.
- **Append-only forever.** No rewrite, truncate, or compaction verb, on either
  side. Keeping only the last record per key is the READER's fold, not a file
  operation.
- **A journal write failure aborts the run.** Resume does not weaken this.
- **Payloads are domain-shaped.** The driver-side fold is generic over
  `(kind, key, payload)`; interpreting a payload stays in the authored harness.
  dev-tree's `split`/`outcome`/`replan`/`rebase`/`escalation` schema never
  appears in Rust.

---

## 1. The injection seam

PRD sub-question (ii) leans **inject, not read through an effect** — "resume
should not be able to forget to look". The mechanism has to satisfy three
things at once: the folded map reaches the authored `loop`; `record` stays
write-only; and no existing harness breaks (twelve files declare
`loop :: State -> Harness State`, most of them outside this lane's blast
radius).

### Decision: an opt-in second entry point, `resumeLoop`, selected by the driver

The universal entry is unchanged. `state_cross` already splices boot-time
bindings into the loop fragment (`__selfHarnessState`, `__operatorMsg`); this
adds a third and, when a fold exists, calls a wider entry:

| boot condition | spliced helpers | compiled entry |
|---|---|---|
| no journal, or an empty fold | `__selfHarnessState` (+ `__operatorMsg`) | `Loaded.loop __selfHarnessState` |
| a non-empty fold, harness declares `resumeLoop` | ↑ plus `__selfHarnessResume` | `Loaded.resumeLoop __selfHarnessResume __selfHarnessState` |
| a non-empty fold, harness does NOT declare `resumeLoop` | — | refuse at boot: `DriverError::ResumeEntryMissing` |

```haskell
resumeLoop :: ResumeFold -> State -> Harness State
resumeLoop _ = loop           -- the trivial, honest opt-in
```

Three properties this buys:

- **A resumed run cannot silently forget to look.** A journal with entries and
  a harness with no `resumeLoop` is a loud boot refusal naming the harness file
  and the journal path — not a run that quietly redoes finished work. That is
  the "should not be able to forget" property the PRD wanted, realized as a
  refusal rather than as a type.
- **Every existing harness is untouched.** A fresh run compiles exactly the
  entry it compiles today, byte for byte, so the twelve `loop`-only harnesses
  and their tests keep working with no edit.
- **`record` stays write-only.** The authored program never reads the journal;
  it receives an already-folded value the driver built. `Tidepool.Journal`'s
  module doc needs no amendment — it already says folding-and-injecting is the
  driver's job, which is exactly what happens.

**Detection is structural, not a GHC-error match.** `HarnessSource` already
derives `answerer_imports` by reading the file's import lines; it gains
`declares_resume_entry: bool` from the same kind of scan (a top-level
`resumeLoop` binding — `^resumeLoop\b` in the source, multiline). The scan
picks the entry; GHC is still the real check (a mis-scanned harness fails its
compile the ordinary way). No driver code parses a GHC error message.

**Injection is one-shot, at boot.** The driver holds
`resume_fold: Option<ResumeFold>` and `take()`s it when composing the FIRST
cycle's entry after boot. Cycle 2 onward compiles the ordinary `loop` entry —
the fold describes what a crashed process had already done, and re-handing it
to a later cycle would be handing it stale news. This also makes idempotence
trivial: there is exactly one moment a fold can be consumed.

### Rejected alternatives

- **Widen `loop`'s arity for everyone.** Touches twelve harness files
  including `examples/`, for a parameter eleven of them ignore.
- **A read-side `Resume` effect (`resumeFold :: M Value`).** Works, and stays
  inside the write-only lock (a distinct effect, not a `Journal` read verb) —
  but it is exactly the "harness forgets to call it" shape the PRD leaned
  against, and a forgetting harness would silently redo everything.
- **Splice the fold and let `loop` find it.** A turn-module binding is not in
  scope inside the separately-compiled `Loaded` module. Not available.

---

## 2. Run identity: which segments a resumed run folds, and which one it
   appends to

A resumed run is a different process from the one that crashed, so
per-process naming (`journal-{ts}-{pid}.jsonl`) would fold nothing and orphan
the prior file. Run identity has to outlive the process, and it has to exist
BEFORE the first `record` (a crash in cycle 1 leaves no checkpoint, so the
checkpoint cannot carry it).

**One journal file per PROCESS, not per run.** A run id owns an ORDERED SET
of journal segments (`journal-<runId>.<seg>.jsonl`, `seg` a plain decimal
ordinal — numeric order, never lexicographic, so segment 10 sorts after
segment 9), one per process that run survives. A resumed process never
appends to a segment a prior process left; it always opens a fresh one. This
closes the torn-tail hazard §3 used to carry as an open question: the
crashed process's segment is sealed forever with its torn tail as its
genuine last line, so `load_journal`'s "a torn FINAL line is the crash
point, skip it with a warning" contract is unconditionally true per segment,
never merely true until the next process's first append lands on the same
physical line.

**The run lease** — `<log_dir>/run-current.json`, written at boot, before any
handler is wired:

```json
{"runId": "20260817-101112-48213", "pid": 48213, "startedAt": "…"}
```

Deliberately absent: a segment path. The lease is a statement about the
RUN — the identity a crash must survive — and a segment index is a fact
about one PROCESS within that run. Recording a segment in the persisted
lease would make the lease a statement about whichever process last wrote
it, which is exactly the property that did NOT survive the crash in the
single-file design. Instead, segment allocation ENUMERATES the directory
every time — fresh boot or resume alike — rather than trusting a cached
answer: one past the highest segment ordinal already on disk for the run
id, or `0` when the run id owns none yet. The durable lease answers "which
run"; the directory listing answers "which segments" — two questions, two
places, neither able to go stale relative to the other because the second
is never cached.

| boot condition | behaviour |
|---|---|
| no lease | mint `runId` from `{ts}-{pid}`, write the lease, allocate segment 0, empty fold |
| a lease | **resume**: same `runId`, allocate the next unused segment ordinal, fold every existing segment |
| `run_loop` returns normally | retire the lease: rename to `run-{runId}.json` (retained, never deleted — PRD sub-question (iii)'s "retained like worktrees"), so the next boot mints a fresh run |
| the process crashes | the lease survives → the next boot resumes |

Every segment a run ever writes is retained, appended to by exactly one
process each. Nothing is ever rewritten, and nothing is ever deleted.

**Seq continuity across handler instances.** `JournalHandler`'s counter starts
at 0 per instance and deliberately does not read the disk. A resumed run whose
appends restarted at 0 would be indistinguishable from the prior process's
first entries under a same-seq fold, so the driver — which HAS just folded
every existing segment — seeds it: `JournalHandler::resuming(path, next_seq)`
where `next_seq` is `max(seq) + 1` over every loaded entry (not just the
entries that survive the fold — see §3). The handler's documented
"continuity is a fold-API/driver concern" is honored, not contradicted. `seq`
is written run-global monotonic in practice by this seeding, but — per §3 —
is provenance, not what the fold sorts on.

---

## 3. The fold

```rust
// tidepool-harness/src/selfharness/resume.rs
pub struct ResumeFold {
    run_id: String,
    entries: BTreeMap<(String, String), JournalEntry>,   // (kind, key) → last
    max_seq: Option<u64>,   // over every FOLDED-FROM entry, not just survivors
}
```

- **Keyed by `(kind, key)`, not `key` alone.** dev-tree records `split`,
  `outcome`, `replan`, and `rebase` under the SAME key (the branch name);
  `last_by_key` would collapse a branch's split under its outcome and lose the
  recorded plan. `tidepool-handlers`' fold API gains `last_by_kind_key`
  alongside the existing `last_by_key` (which stays — it is the honest answer
  to a different question).
- **Winner is the last entry in PHYSICAL WRITE ORDER, never `seq`.** Once a
  run spans several segments, "physical order" means the concatenation of
  every segment's entries in segment order, each segment's own entries
  already in that segment's append order — the exact durable byte sequence a
  crash leaves behind. `seq` is written to every entry as PROVENANCE and
  stays run-global monotonic in practice (§2's seeding), but is deliberately
  NOT what the fold sorts on: folding on `seq` would let a foreign,
  hand-edited, or mis-seeded segment carrying a `seq` that contradicts
  physical order silently INVERT the result — not merely lose an entry, but
  pick the wrong one as the winner. Position can't be inverted that way. A
  practical consequence: two entries at the same `(kind, key)` no longer need
  tie-breaking logic — the physically-last one always wins, `seq` or no
  `seq`.
- **Idempotent.** `fold(fold(entries)) == fold(entries)` — folding an
  already-folded set (at most one entry per `(kind, key)`, so there is
  nothing left for position to disambiguate) is a no-op. Folding the
  identical durable bytes twice — the operation a resumed boot performs on a
  run whose segments have not changed since the last boot — always yields the
  same map.
- **`max_seq`/`next_seq` scan every entry folded FROM, not just the
  survivors.** The fold's winner is positional, so an overwritten entry can in
  principle carry a higher `seq` than the entry that overwrote it (the same
  foreign/mis-seeded case above). Deriving `max_seq` from the survivors alone
  would then under-count, and a resumed segment seeded from that undercount
  could allocate a `seq` some earlier, dropped entry already used.
- **Torn-tail handling is `load_journal`'s, unchanged, and now
  UNCONDITIONALLY true per segment.** A torn FINAL line is skipped with a
  warning (the crash-mid-append case this lane exists for); a torn line
  anywhere earlier is `TornMidFile` and fails the boot loudly. Resume does not
  soften either — see §2's segmentation for why no segment but the crashed
  one can ever carry a torn tail.

**Resolved — the torn-tail hazard, closed by segmentation (was: "the torn-tail
tolerance is one boot deep").** The single-file design's hazard: a torn tail
has no trailing newline, and the journal was append-only-forever onto ONE
file, so a resumed run's first append landed on the SAME line as the torn
bytes and the two merged into one unparseable line. The boot that folds a
torn tail was fine; the boot after that was not — two or more appends left
the merge mid-file (`TornMidFile`, refusing the boot), and exactly one append
left it last (silently absorbed as a torn tail, quietly dropping a durable
record). Every fix touched a locked decision (truncating the tail, or
softening `load_journal`) — until the file boundary itself moved: a resumed
process now always opens a FRESH segment (§2) rather than appending into the
crashed process's file, so the merge this hazard depended on can never
happen, at any boot depth. `load_journal`'s torn-tail contract is untouched;
it is simply true unconditionally now instead of true-until-the-next-append.
Pinned by `selfharness::resume::tests::a_segment_with_a_torn_tail_never_poisons_a_later_boot`
(pure fold, several boots) and
`selfharness_persistence::a_torn_tail_never_poisons_a_later_boot_through_the_driver_seam`
(the same property through the real `open_run_journal` seam) —
`appending_after_a_torn_tail_is_survivable_exactly_once`, which pinned the old
one-boot-deep limit, no longer describes a property this design has and was
replaced rather than kept as a stale acceptance.

**Wire shape** (Rust `to_json` → the `__selfHarnessResume` splice →
`Tidepool.Resume`'s decode). Entries emit sorted by `(kind, key)` so the
splice is byte-deterministic and the compile memo hits:

```json
{ "runId": "…",
  "entries": [ {"seq": 7, "kind": "split", "key": "dev-tree/a", "payload": {…}} ] }
```

A list, not an object-of-objects: a `(kind, key)` pair is not a JSON key, and
a flat list decodes through the stdlib's hand-written `FromJSON` with no Map
instance question. `Tidepool.Resume` builds whatever lookup structure it wants
on the Haskell side.

---

## 4. Adopt-and-verify (the authored half)

Locked: **a commit found in a retained worktree is never redone blind and
never trusted blind.** Resume runs the ladder on what it finds.

The whole of this lives in the authored harness — it reads domain payloads
(`split`'s `scaffoldHead`, `outcome`'s receipt) and runs domain checks
(`nodeChecks`). The driver contributes nothing but the fold.

For each branch with a recorded `split` and NO recorded `outcome`:

1. `lookupWorktree` / `listWorktrees` rebinds the retained worktree by id
   (PRD: "retained worktrees rebind, not recreate" — `spawnSpecIn`, not
   `createWorktree`). A tree a human removed is `WorktreeLost`: surfaced as
   data, never recreated.
2. `worktreeHead` versus the `scaffoldHead` in the recorded split payload. Not
   moved ⇒ no orphaned work; that node is genuinely unstarted and unfolds
   normally.
3. Moved ⇒ orphaned work exists. Run `runChecks` (the orchestrator's own
   checks, in that worktree, at that sha) plus `boundaryViolations`.
4. **Pass ⇒ adopt**: synthesize the `Outcome` the crashed process never got to
   record, `record "outcome"` it, and fold it as if the run had produced it.
5. **Fail ⇒ surface as data**: a `Failure` carrying the failing checks and the
   sha, flowing through the algebra like any other failed child — which is
   where the ordinary `OnFailure` policy (`Retry`/`Replan`/`AskOperator`/
   `Abandon`) already lives. Never a silent redo, never a silent adoption.

Two refinements the ladder needs once it meets a real plan, both in
`harness-dogfooding/dev-tree/Harness.hs`:

- **Steps 3–5 do not judge; `foldLadder` does.** The verify step stamps a
  `FoldReceipt` from what it observed and hands it to the harness's existing
  ladder, so an adopted commit is judged by the same rungs in the same order as
  a fold the run performed. One judge, no second verdict to drift.
- **What the orphaned work means depends on the plan.** A LEAF's commit is its
  whole fold. An INTERIOR node with no recorded split has an orphaned SCAFFOLD,
  so a passing verification unfolds FROM it rather than adopting it as an
  outcome — its children still have to run. An interior node WITH a recorded
  split is adopted only when the integration is provably complete (every child
  in the recorded plan recorded a `Done` outcome, and every one of those
  branches is an ancestor of the node's HEAD); a partial integration is
  replayed, which is safe because merging an already-merged branch is a no-op.

---

## 5. What a resumed dev-tree run skips

`resumeLoop` re-enters the same hylo with a coalgebra and algebra wrapped by
the fold:

- **Recorded `split` ⇒ replay, do not re-ask.** `decompose` returns the
  recorded plan and child names instead of spawning the scaffold worker. This
  is the load-bearing one: a nondeterministic re-plan orphans every completed
  child beneath it.
- **Recorded `outcome` ⇒ stands.** That subtree is not re-entered at all; the
  recorded receipt is the algebra's input.
- **Recorded `replan` ⇒ the amendment applies at re-unfold.** dev-tree v2
  journals a `Replan` decision under kind `"replan"` for exactly this consumer
  (PRD: "a resumed run reads the journaled amendment and unfolds from there").
  A branch with a `replan` newer than its `split` re-unfolds under the amended
  plan, not the original one.
- **Recorded `rebase`/`escalation` ⇒ carried into the receipt** so a resumed
  run's summary is not missing the prior process's rebase steps.
- **Nothing recorded ⇒ ordinary work**, after the adopt-and-verify pass above.

Resuming a run whose journal records an outcome for the root does no work at
all: the root's outcome stands, so the hylo folds it and returns.

---

## 6. Waves

**Wave 0 (scaffold, this commit).** This document + `haskell/lib/Tidepool/
Resume.hs` (the `ResumeFold`/`ResumeEntry` types, their hand-written
`FromJSON`, and the generic lookup vocabulary). Both halves fork from here, so
the wire shape cannot drift between them.

**Wave 1 — driver-side boot fold (Rust).**
`last_by_kind_key` + `JournalHandler::resuming` (tidepool-handlers);
`selfharness::resume` (fold, wire encode, the run lease);
`state_cross::resume_in`; `HarnessSource::declares_resume_entry`; driver entry
selection + `DriverError::ResumeEntryMissing`; one non-desyncable wiring seam
(`open_run_journal`) replacing the binary's hand-wired handler; unit tests
(idempotence, order-insensitivity, torn tail, seq continuity, lease lifecycle)
and an acceptance test over a `resumeLoop`-declaring fixture.

**Wave 2a — dev-tree v2 consumes it (Haskell).** `resumeLoop`, skip-recorded
split/outcome, replan amendments at re-unfold, adopt-and-verify.

**Wave 2b — the crash-resume acceptance test (Rust).** A cycle that dies
mid-flight after k of n recorded steps (`tests/fixtures/CrashResumeHarness.hs`
reaches a Console verb the crashed driver has no handler for — the same durable
residue a kill between two appends leaves: flushed journal lines, no committed
checkpoint, an unretired lease), resumed, doing exactly the n-k remainder with
seqs continuing past the crashed run's; then the retire/mint direction (a normal
completion retires, and the next boot mints a run whose journal folds nothing
while the finished run's stays intact). Plus the torn-tail legs above, folded
from hand-written journals with no compile. All in
`tidepool-harness/tests/selfharness_persistence.rs`.

**Wave 3 — journal segmentation, closing the torn-tail hazard (Rust).**
Resolves this document's former "the torn-tail tolerance is one boot deep"
open question (§3) by moving the file boundary: a run id owns an ORDERED SET
of segments, one per process, instead of one file every process appends to.
`selfharness::resume` gains segment naming/allocation/enumeration
(`segment_path`, `list_segments`, `next_segment_ordinal`, `fold_run_journal`);
`RunLease` drops its segment field (a statement about the run, not a
process — §2) and `AcquiredLease` gains one (this process's own, freshly
allocated every boot); `open_run_journal` folds every segment for the run id
and appends only to the one this process was allocated. Plus two adjacent
fixes an external review surfaced in the same file this wave already had
open: `JournalHandler::append` now serializes the whole open+write+fsync
under a lock shared by every clone (the old "one `write_all` is one atomic
`write()`" argument doesn't hold on a short write) and calls `sync_data()`
instead of the no-op `File::flush()`, so a `record` that returns `Ok` is
actually durable; and the fold now sorts on PHYSICAL WRITE ORDER
(segment order, then in-segment append order) rather than `seq`, since a
foreign or mis-seeded segment could otherwise invert the result under a
max-seq fold — `seq` stays on the wire as provenance only. Replaces
`appending_after_a_torn_tail_is_survivable_exactly_once` with tests pinning
the new, stronger property (several boots past a torn tail, both at the pure
`selfharness::resume` layer and through the real `open_run_journal` seam).
