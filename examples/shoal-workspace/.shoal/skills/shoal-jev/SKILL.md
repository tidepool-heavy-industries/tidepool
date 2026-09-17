---
name: shoal-jev
description: Ask Jev, the cheap judgment model, inside a Haskell cell — packets, pools, calibrated options, and gating an action on a policy. Load when a cell needs a semantic decision (triage, routing, relevance, a review gate) instead of another model round.
---

Jev is a fast "System 1" call beside your own reasoning: about 200 ms, a small
fraction of a cent, no per-run cap worth rationing. Call it per item, per file,
per candidate, inside a loop. Ten Jev calls that save one model round are a win;
one Jev-dense cell that replaces five read-then-judge model rounds is a much
bigger one. A judgment is evidence, not authority, and it never generates the
action: a choice selects a payload you already built, and that payload runs.

`import qualified Jev.Operators as J` is in scope; `:=` and `:&` read
unqualified. A cell that calls Jev leads with two pragmas:

```
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
```

`J.ask1 state question` asks one; `J.ask state packet` asks a whole packet in
one round trip. Both return `Either J.JevError _`. Read the error and fall back
to your own policy; never retry blindly.

## Be dense

One packet per semantic boundary. Gather the evidence with a shell listing and
one read per candidate, bind short previews, then ask everything the evidence
can answer at once. Ask again only when a read, a command or a reply has
changed the world. This is two cells for work that otherwise costs five model
rounds, and only the shortlist — never the file text — reaches your context.

```haskell
listed <- Cmd.stdout <$> Cmd.run [bash|ls|]
let names = either (const []) T.lines listed
previews <- forM names $ \n ->
  (n,) . either (const "") id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [n] [bash|head -n 20 -- "$1"|])
```

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let files = J.pool #files [(n, String p, n) | (n, p) <- previews]
let packet =
      #files := files
        :& #enough := J.noul "Is a 20-line preview enough to judge each file, or does judging need the whole file?"
        :& #worth_reading := J.eachIn files (\r -> #keep := J.askAbout r "Worth reading in full for this review?" :& J.Nil)
        :& J.Nil
answer <- J.ask (J.state (object ["task" .= ("triage files before a focused review" :: Text)])) packet
fmap (\r -> let a = J.answers r in (a.enough.yes, [(k, s.keep.yes) | (k, s) <- a.worth_reading])) answer
```

Keep previews short (`T.take 2000`) and bind the previews, not the files: every
bound value is observed, and six full files exhausted the observation budget
before the Jev call in one measured run.

## Reading the answer

Answers are data. A choice cell has `.key`, `.mass`, `.margin`, `.confidence`
and `.masses`; a noul has `.yes`; a score has `.nearest`, `.expectation`,
`.masses` and `.confidence`; a `Selected` has `.key`. A whole `Response`
displays as an object of those fields, so ending a cell with the bound `answer`
shows every distribution — do that the first few times you write a packet, then
project what the next decision needs. `J.contenders floor a` reads every
alternative above a mass floor, best first; a near tie is a typed outcome worth
branching on. `J.handle a.chosen handlers` eliminates a choice exhaustively.

## Calibration (measured, 262 calls)

- A mass of 1.0 means no option in the pool competes. Rewording does not move
  it. If the 1.0 surprises you the competing option is missing; if it does not,
  the question was not worth asking.
- Options describe the **condition** that makes them apply, in terms of fields
  the state has — not the action ("launch the child now", which flattens toward
  the prior) and not the argument ("despite existing coverage", which steers).
- Rivals come from the evidence, not from your own shortlist, or the pool
  inherits your ranking.
- Gate on confidence first. Both wrong answers over 42 labelled choices sat
  below 0.25 confidence. The named policies are the measured thresholds:
  `J.routing` (which file, which skill), `J.spawning` (launch a worker, choose
  an approach, accept a reviewed test-passing diff), `J.merging` (merge, stop,
  anything with a receipt — and any diff nobody reviewed). Pick by stakes; do
  not hand-tune a fourth.
- Gate on the artifact — the diff and the test output — never on the child's
  own report. With the artifact present a lying report moved no answer more
  than 0.08; with only the report, confidence fell to 0.07.
- State plus questions is capped at 32k tokens, and a 30k whole diff already
  drops confidence. Ask file questions on `git diff --stat`, content questions
  per file on its hunks, files in parallel.
- Always chain an exit: a no-match key, or a condition that hands back to the
  model. A `choice` without one still picks something.

## A gate is a checklist, not "is this enough"

Write the gate as an ordinary three-option `choice`: every named item present,
at least one named item absent, or the state contradicts one of them. Name the
items. "Is the report sufficient?" measures nothing; the checklist form moved
confidence from 0.58 to 0.92 with zero variance over eight repeats. A judgment
noul works the same way — state the yes condition and the no condition in the
question, or attach them with `J.about`.

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let diffStat = "src/Retry.hs | 24 ++++--\ntests/RetrySpec.hs | 31 +++++" :: Text
let testOutput = "PASS 14 tests, 0 failures" :: Text
let gate = J.choice "Which of these describes the candidate?"
      (J.alt #all_present "The diff touches only the named paths, the test output shows every named check passing, and a reviewer recorded the scope those checks establish" ()
        J..| J.alt #one_absent "At least one of the named paths, the passing check output, or the recorded review scope is missing" ()
        J..| J.alt #contradicts "All three are present, but the diff and the test output disagree about what was checked" ())
answer <- J.ask1 (J.state (object ["owned_paths" .= (["src/Retry.hs", "tests/RetrySpec.hs"] :: [Text]), "diff_stat" .= diffStat, "test_output" .= testOutput, "review_scope" .= ("retry bounds only" :: Text)])) gate
case answer of
  Left err -> "jev unavailable: " <> T.pack (show err)
  Right a -> either (\doubt -> "hold: " <> T.pack (show doubt) <> "; " <> J.explain J.spawning a) J.selectedKey (J.accept J.spawning a)
```

`J.spawning` is the policy for a reviewed, test-passing diff. Use `J.merging`
for a diff nobody reviewed, or for anything that leaves a receipt. `J.explain`
states in one line why the answer was accepted or doubted; put that in the
notice you send, not the raw distribution.

## The payload is the continuation

Alternatives carry the thing that runs, not a key string you later interpret.
Build the prepared continuations, let Jev select one, and run the selection.
The wording is model-facing; the payload is yours.

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let failing = "session::retry_is_bounded" :: Text
let continuations =
      J.alt #inspect_by_hand "The output names neither a single test nor a target" (pure ("reading the failure by hand" :: Text))
        J..| J.many
          [ ("rerun_one", String "The output names exactly one failing test in one target", either (const "rerun unavailable") id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [failing] [bash|echo "would rerun $1"|]))
          , ("rerun_suite", String "The output names failures in more than one target", either (const "suite unavailable") id . Cmd.stdout <$> Cmd.run [bash|echo "would rerun the suite"|])
          ]
answer <- J.ask1 (J.state (object ["test_output" .= ("FAIL [ 0.2s] tidepool-runtime session::retry_is_bounded" :: Text)]))
  (J.choice "Which prepared continuation matches this test output?" continuations)
next <- either (const (pure "jev unavailable; inspecting by hand")) (\a -> J.handle a.chosen (#inspect_by_hand id J..| J.onMany (\_ action -> action))) answer
next
```

A pool declares one candidate set for several questions in the same packet:

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let candidates = [("retry", "src/Retry.hs: retry loop and backoff" :: Text), ("fetch", "src/Fetch.hs: HTTP client and timeouts")]
let files = J.pool #files [(k, String d, k) | (k, d) <- candidates]
let packet =
      #files := files
        :& #best := J.choice "Which file explains the timeout?" (J.manyFrom files J..| J.alt #none "No file in this set is on the timeout path" "")
        :& #per := J.eachIn files (\r -> #relevant := J.askAbout r "Is this file on the path the timeout takes?" :& J.Nil)
        :& #fixed := J.given "The retry loop changed yesterday" (J.noul "Is the timeout already fixed?")
        :& J.Nil
answer <- J.ask (J.state (object ["failure" .= ("fetch times out after 3 retries" :: Text)])) packet
fmap (\r -> let a = J.answers r in (fmap J.selectedKey (J.accept J.routing a.best), [(k, s.relevant.yes) | (k, s) <- a.per], a.fixed.yes)) answer
```

When several items may each qualify, ask one `noul` per item over a pool, not
one `choice` over the items: a choice distribution is relative, a noul per item
is not. Measured on the pool cell above: `best` flips when the descriptions are
dropped or the question is rephrased while the per-file nouls hold, so derive
`best` in code from the nouls rather than asking for it. Use a choice alongside only when exactly one must win. A packet's
questions cannot see each other's answers; ask a dependent question under
`J.given premise` instead of making a second call.

When the answer resolves nothing useful, return to ordinary reasoning with the
packet's evidence still bound in the cell. Nothing is recomputed, and a
recurring handback is the signal to write that branch by hand. `doc jev` is the
same material in fallback form.

## Playbook (measured 2026-09-17, 365 calls, on a real Shoal run's candidates)

1. State is an object with named fields; questions name the fields in backticks. Never paste a child's narration into an evidence field.
2. Review gate, `J.merging`: state `{owned_paths, acceptance_checklist: [..], base, candidate, diff_stat, hunks, test_output}` (no `brief` prose: ablation showed the gate reads hunks and test output only, and the checklist already lives in the options; `diff_stat` stays for the `#covered` tripwire and the code coverage check), where `base` and `candidate` are the OIDs and `diff_stat`/`hunks` come from your own `git diff <base>..<candidate>` in your checkout, never from the child's report (the child's file list is a claim to check, not the evidence). Before sending, check coverage in code: every file in the stat has a hunk, and the stat's insertion and deletion counts equal the hunks' line counts; if either fails, do not ask the gate. Keep one tripwire Noul in the packet, `#covered := J.noul "Does `hunks` contain a hunk for every file named in `diff_stat`?"`, for any state assembled by hand. A confident `all_present` never means the evidence was complete: with every hunk removed and the stat intact the gate still said `all_present` at 0.62 to 0.82. Completeness is your job and it is a git command. One `J.choice "Which statement describes the candidate?"` with `#all_present "Every item of the checklist holds: <items joined by ;>."` (every option enumerates its conditions: shortening `item_missing` to "some checklist item does not hold" flipped a clean candidate from 0.86 to 0.54), `#item_missing "At least one item does not hold: a changed file outside the owned file, a failing or missing owned test, a deleted or weakened test, a remaining todo!(), or an implementation that does not match the goal."`, `#conflicting "The items are all present but contradict each other, for example the report claims a test passes that the test output shows failing."`. `J.accept J.merging a.gate`; on `Left`, the doubt names the checklist item to send back. On five real candidates plus five planted-bad ones: zero false accepts, no good candidate doubted at any policy.
3. Never ask "is this sufficient" or "does this need repair" (a bare Noul sat under 0.2 on everything). Ask the literal facts and derive the judgment in code.
4. Per-child triage, one call for all children, `test_output` compacted to the `test …`, `panicked at` and `test result` lines: `#real := J.askAbout r "Does any test whose path is inside `owned_file`'s module fail in `test_output`?"`, `#scope := J.askAbout r "Does `files_changed` include any file other than `owned_file`?"`; `needs_repair = a.real.yes > 0.5 || a.scope.yes > 0.5`. Literal per-item questions were right at every pool size; judgment questions were wrong in isolation and drifted right only pooled.
5. Report honesty is a tie-breaker at 0.20: `#honest := J.noul "Is every claim in `worker_report` supported by what `test_output` and `files_changed` actually show?"`. A terse "Done." sits near 0.4; a lie at 0.02. Gate on the literal `own_tests_pass` first.
6. Risk of a diff is a Noul battery, not a Score: `touches_outside_ownership`, `changes_public_item_used_elsewhere`, `deletes_or_weakens_test`, `leaves_todo`; rank merges by the count in code. A Score whose confidence is near 0 cannot establish an ordering: inspect `a.masses` and whether the rung descriptions overlap (on a real diff, four situation-shaped rungs gave 0.38/0.22/0.29/0.11 because two of them both applied). Reading the masses diagnoses it; it does not recover a ranking.
7. Use a Score only for an ordered ladder of exclusive situations; rungs describe what a thing on that rung looks like, never low/medium/high.
8. Use a Choice only when exactly one option wins; options describe the condition that makes them apply, in state-field terms. Not the action, not the argument.
9. A 1.0 means nothing else in the pool competes. If that surprises you, the missing option is the one to add.
10. Policies by stakes: `J.routing`, `J.spawning`, `J.merging`. Gate on `a.confidence` first.
11. `check.sh` output never needs Jev: exit 0 is green; a fmt diff → `cargo fmt`; a clippy lint name → `cargo clippy --fix` up to three times, then a model; `not yet implemented` → a stub; `assertion … failed` → a real failure. Only unmatched output goes to Jev. `check.sh` runs fmt before clippy before tests, so expect them in that order.
12. Scope rule before any classification: a failing test outside the child's owned module is expected, not a wakeup.
13. Skill pre-load: `#first := J.choice "Which skill should the root load first for `moment`?" roster` plus one `helps_<skill>` noul each; load at `a.first.confidence >= 0.70`, else read the roster yourself (16 of 18 real moments exact; no miss at or above 0.70).
14. Keep a dense cell under about 30 compact items; six full test outputs already exceed the 32k state cap.
15. Literal questions are stable across phrasings (under 0.03); judgment questions move up to 0.45. Pair only `honest` and `stuck` and take the minimum.
16. Read `a.key`, `a.mass`, `a.margin`, `a.confidence`, `a.masses`; `a.yes`; `a.expectation`, `a.nearest`. A bare `answer` displays. No projection lambdas.
17. Every call is worth a ledger row: the questions, the answers as above, `J.resolvedModel`, and the action taken.
18. When Jev returns `Left`, fall back to reading in order and keep going; a recurring handback is a missing branch to write by hand.
19. Jev is evidence, not authority: the checklist gate agreed with five fresh reviewers on five real candidates; it replaces the reviewer only where the checklist is the whole contract.
