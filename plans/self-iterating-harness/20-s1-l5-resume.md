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

## 2. Run identity: which journal a resumed run folds, and appends to

Today `tidepool-web/src/bin/tidepool-selfharness.rs` mints
`journal-{ts}-{pid}.jsonl` per PROCESS. A resumed run is a different process,
so per-process naming would fold nothing and orphan the prior file. Run
identity has to outlive the process, and it has to exist BEFORE the first
`record` (a crash in cycle 1 leaves no checkpoint, so the checkpoint cannot
carry it).

**The run lease** — `<log_dir>/run-current.json`, written at boot, before any
handler is wired:

```json
{"runId": "20260817-101112-48213", "journal": "…/journal-20260817-101112-48213.jsonl", "pid": 48213, "startedAt": "…"}
```

| boot condition | behaviour |
|---|---|
| no lease | mint `runId` from `{ts}-{pid}`, write the lease, fresh journal file, empty fold |
| a lease, journal present | **resume**: same `runId`, APPEND to the same file, fold it |
| a lease, journal missing | resume with an empty fold, keep the `runId` (the file is created on first append) |
| `run_loop` returns normally | retire the lease: rename to `run-{runId}.json` (retained, never deleted — PRD sub-question (iii)'s "retained like worktrees"), so the next boot mints a fresh run |
| the process crashes | the lease survives → the next boot resumes |

One file per run id, appended across however many processes that run takes.
Nothing is ever rewritten, and nothing is ever deleted.

**Seq continuity across handler instances.** `JournalHandler`'s counter starts
at 0 per instance and deliberately does not read the disk. A resumed run whose
appends restarted at 0 would be indistinguishable from the prior process's
first entries under a max-seq fold, so the driver — which HAS just folded the
file — seeds it: `JournalHandler::resuming(path, next_seq)` where `next_seq`
is `max(seq) + 1` over the loaded entries. The handler's documented "continuity
is a fold-API/driver concern" is honored, not contradicted.

---

## 3. The fold

```rust
// tidepool-harness/src/selfharness/resume.rs
pub struct ResumeFold {
    run_id: String,
    entries: BTreeMap<(String, String), JournalEntry>,   // (kind, key) → last
}
```

- **Keyed by `(kind, key)`, not `key` alone.** dev-tree records `split`,
  `outcome`, `replan`, and `rebase` under the SAME key (the branch name);
  `last_by_key` would collapse a branch's split under its outcome and lose the
  recorded plan. `tidepool-handlers`' fold API gains `last_by_kind_key`
  alongside the existing `last_by_key` (which stays — it is the honest answer
  to a different question).
- **Winner is MAX SEQ, not file position.** Strictly stronger than "last in
  the file": folding is then order-insensitive by construction, so an
  interleaved append order folds identically. Ties (impossible from one
  seq-stamped writer, possible from a hand-written fixture) break on file
  order, documented, so the fold is total.
- **Idempotent.** `fold(fold(entries)) == fold(entries)` — a pure function of a
  set of entries. Folding twice, or folding a file a resumed process has since
  appended to, is the same map plus whatever is genuinely new.
- **Torn-tail handling is `load_journal`'s, unchanged.** A torn FINAL line is
  skipped with a warning (the crash-mid-append case this lane exists for); a
  torn line anywhere earlier is `TornMidFile` and fails the boot loudly. Resume
  does not soften either.

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

**Wave 2b — the crash-resume acceptance test (Rust).** Scripted agents, killed
between steps; the resumed run appends only the delta.

**Wave 3 — rebase onto trunk, full verify, submit.**
