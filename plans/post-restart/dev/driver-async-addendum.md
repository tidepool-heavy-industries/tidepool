# Addendum to driver-async: the stale-checkpoint boot crash

**Priority: land this FIRST, in its own commit, ahead of the async
conversion, so the two are separable at the fold.** Everything else in
`driver-async.md` stands unchanged, including the contention rules.

The first live dogfood session died at boot on code you already own —
`restore` (~805) and `run_loop` (~773), both already on your conversion list.
That is why this comes to you instead of a third dev.

## What happened, from the live log

1. `checkpoint.json` existed from an earlier run of a DIFFERENT harness source
   (fingerprint `fcbd20d2594c4426`; state shape
   `lastDecision/loopCount/mode/notes`, no `target` key).
2. Boot against the current wizard source (`dff30457c5b175a7`) DETECTED the
   mismatch — it emitted `Event::HarnessSourceChanged` — and then restored the
   stale state anyway.
3. Decode of that state against the new `State` type failed:
   `[JIT] runtime_error kind=2 (UserError) "TIDEPOOL_STATE_DECODE_FAILED: key
   \"target\" not present"`, then four `[CASE TRAP]`s in `render_1_*`, then a
   dead process. No listener on 4600 — the operator GUI died with it.

Two separate defects. Fix both.

## Defect 1 — a fingerprint mismatch must discard, not restore

`restore`'s doc comment states the current design outright: *"A restored
checkpoint whose fingerprint disagrees does not block the restore — a harness
file is expected to change across a self-iteration run — but is reported via
`Event::HarnessSourceChanged` so a later `DriverError::StateDecode` is
diagnosable rather than mysterious."* That was a deliberate call, and it is
the bug: the restore path emits the event and then ignores it.

New rule, on mismatch (`checkpoint.harness_source != source.fingerprint`):

- keep emitting `Event::HarnessSourceChanged` — it is the durable record;
- add a loud `log::info!` naming BOTH fingerprints and stating explicitly that
  the persisted state was DISCARDED and the run starts fresh;
- return `Ok(None)` — the state does not come back;
- still adopt `checkpoint.generation`, so the generation sequence stays
  monotonic across the restart (the crash-recovery test asserts 1 → 3; do not
  break it);
- do NOT adopt `checkpoint.compaction` — that summary describes a different
  harness's loop, so it is `None` on this path.

Rewrite that doc comment to state the new rule. Describe what IS — no
"formerly", no narration of the old behavior. That goes in the commit message.

## Defect 2 — a state-decode failure must never take the process down

Defense in depth: whatever the fingerprint check misses (a hash collision, a
hand-edited checkpoint, a same-source edit that changes the `State` type
without changing the file's fingerprint), a decode failure must degrade, not
kill.

The typed error already exists and already reaches you: `map_run_error` (~81)
recognizes `state_cross::STATE_DECODE_SENTINEL` and produces
`DriverError::StateDecode(detail)`. The only reason the process died is that
`run_loop` propagates it —
`let outcome = self.run_one_cycle(source, state_json.as_ref())?;` → out of
`run_loop` → the bin's `driver.run_loop(&source, auto)?` → `main` returns
`Err` → exit.

In `run_loop`: if a cycle fails with `DriverError::StateDecode` **while
`state_json` is `Some`**, log loudly (the decode detail verbatim), set
`state_json = None`, and retry that cycle ONCE from fresh state. If the retry
also fails it is an ordinary cycle error and takes the existing F3 path — do
not add a new ladder rung, and do not loop forever retrying. A `StateDecode`
when `state_json` is already `None` is a real bug in the harness source itself
and propagates as it does today.

Do not touch `SelfHarnessState::Failed`/`Poisoned` semantics. F3 is DONE
(durability wave); you are adding a retry ahead of it, not changing it.

## Fixture — from the real defect

The exact checkpoint that crashed the live session. Create it as a committed
test fixture (fixtures-from-real-defects); do NOT read it from a scratchpad
path at test time.

Put it under `tidepool-harness/tests/fixtures/`, alongside what is already
there. **Check `.gitignore` does not swallow it** — a gitignore-invisible
fixture is a known trap in this repo, and a test that silently skips is worse
than one that fails.

```json
{
  "generation": 1,
  "state": {
    "lastDecision": {
      "action": "observe",
      "confidence": "Medium",
      "rationale": "first loop"
    },
    "loopCount": 1,
    "mode": "Deciding",
    "notes": [
      "observe"
    ]
  },
  "compaction": null,
  "harness_source": "fcbd20d2594c4426"
}
```

## Acceptance

Boot with a wrong-fingerprint checkpoint present → the session comes up FRESH,
serves, and logs the event. At minimum:

- A test that plants the fixture at the driver's `checkpoint_path`, calls
  `restore` with a source whose fingerprint differs, and asserts: it returns
  `Ok(None)`; `Event::HarnessSourceChanged` was observed carrying both
  fingerprints; `last_compaction` is `None`; `checkpoint_generation` adopted
  the file's generation.
- A test that a `StateDecode` on a restored-state cycle retries from fresh
  state instead of propagating out of `run_loop`.

**Mutation-close both** — a green run is not the receipt:

- revert defect 1 (return `Ok(Some(state))` on mismatch) → the first test must
  go RED;
- revert defect 2 (propagate `StateDecode`) → the second must go RED.

Report the exact assertion message each mutant produced.

`selfharness_persistence` is the natural home; `selfharness_compaction_fixes`
also touches this area. Your call. Add whichever binary you use to your
GHC-heavy verification list.

## Known trade-off — state it in your submit note, do not try to solve it

Discarding on mismatch means a self-iterating harness that edits its own file
and then restarts loses its accumulated `State`, even when the state TYPE did
not change. That is a real cost, and it is precisely why the current code
restores anyway.

We take it deliberately: a lost `State` costs a run, a poisoned `State` costs
the process — and the process just died in front of the person driving it. If
it bites, the follow-up is a separate state-SHAPE fingerprint alongside the
source fingerprint. **Do not build that now.**
