# Worked cells

Eight cells that ran against real artifacts, kept because each one shows a
technique rather than a result. They are reference to copy from, not modules to
import — the programs themselves are in `.shoal/Project/`.

Every one of them assumes this helper. Declare it once in your session:

```haskell
sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.quiet (Cmd.run (Cmd.argv args))
  pure (either (const "") id (Cmd.stdout r))
```

`Cmd.quiet` is the point of it. Without that wrapper a cell's result carries
every byte the command printed, and a cell can succeed, pay for its model calls,
and show you almost none of its answer.

## What each one is for

**`10-dispatch.hs`, `11-dispatch-loop.hs` — a typed action dispatcher whose
alternatives carry real commands.** Three steps with no model turn between them:
fetch a commit message, then a diff, then the call sites. The idiom to take is a
payload that is an `Eff` action and a `J.handle` that dispatches into it. The
loop version shows why a fetch loop stops repeating itself — each alternative
names what the state lacks, so satisfying it removes its own reason to be chosen.

A defect is left in deliberately: it truncates each fetch before feeding it back
into its own observations, which can delete the deciding evidence. Fix that first
if you build on it.

**`33-threeway.hs` beside `35-threeway-fair.hs` — the same program before and
after one wording repair.** Keep them together; this is the worked example of
debugging a semantic program. The only difference is that in the second, all four
alternatives describe what the state field contains. The ambiguous case goes from
confidently wrong to right at mass and confidence 1.00. The lesson generalises:
mixed vocabulary across alternatives produces a confident wrong answer, not a
weak one.

**`32-traverse-content.hs` — navigation with contents as branch evidence.** Code
lists the children, the model picks one, code descends. The earlier version of
this scored branches by filename alone and went to the wrong file at 0.51 against
0.47; supplying an excerpt of each branch fixed it.

**`37-reflect-intent.hs` — an agent's own history answering what the diagnostics
cannot.** Three separate named fields for instructions, history and repository
evidence. Evidence alone refuses; the history reaches the right answer; off-task
history returns `unresolved` rather than inventing something.

**`12-termination.hs` — four forms of one question in a single packet over
identical state.** The cheapest way to diagnose a question that is behaving oddly.

**`52-question-self-lint.hs` — a packet that scores your own questions before you
send them.** One axis, "states the deciding fact", separated measured-weak from
measured-strong wordings at 0.20 against 0.45.

## About the fixtures

`fixtures/` holds real recorded output — three failing checks and one worker's
seventeen real steps — from a small demo application this project used as a
dogfood target. They are kept because they are genuine artifacts rather than
invented ones, and because a compiler's own words are what the questions are
about. The paths inside them name that application's files, not this one's.

The cells that read those fixtures run here unchanged. The cells that walk live
git have been repointed at this repository: commit
`7a48345d61ee13f2a803547ae5c05040dc7ae37d`, which changed a function's signature
in `tidepool-actor/src/request/updates.rs` and updated its callers — the same
shape of question the originals were asking. Those have not been re-run since
being repointed, so treat their wording as sound and their numbers as unverified.

Read `.shoal/plans/README.md` for what is specific to working in this repository,
and load the `shoal-jev` skill for the question-writing rules.
