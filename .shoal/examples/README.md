# Worked cells

Six cells kept because each shows a technique, kept because each one shows a
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

**`dispatch-tidepool.hs` — a typed action dispatcher whose alternatives carry
real commands.** The alternative the model picks *is* the command that runs, so
there is no model turn between choosing and fetching. Each alternative also names
what the state currently lacks, which is why a loop built from this does not
repeat itself: satisfying one removes its own reason to be chosen.

It asks about a real change here — commit `7a48345d6`, which changed
`update_request` so a success carries its delivery instead of an `Option` — and
every one of its four commands returns real evidence from this repository.

A defect is left in deliberately: it truncates each fetch before feeding it back
into its own observations, which can delete the deciding line. Fix that first if
you build a loop on it.

**`33-threeway.hs` beside `35-threeway-fair.hs` — the same program before and
after one wording repair.** Keep them together; this is the worked example of
debugging a semantic program. The only difference is that in the second, all four
alternatives describe what the state field contains. The ambiguous case goes from
confidently wrong to right at mass and confidence 1.00. The lesson generalises:
mixed vocabulary across alternatives produces a confident wrong answer, not a
weak one.

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

The cells that read those fixtures run here unchanged, and ask about those
artifacts, which is coherent — the questions are about a compiler's own words and
the fixtures are where those words are.

The cells that walked live git could **not** be carried over by repointing them.
A first attempt substituted a commit and a path mechanically, which left a cell
asking whether an item limit had been requested while reading a commit about
request updates: the artifacts moved and the question did not. Those experiments
are kept exactly as they ran, with their recorded numbers, in
`plans/jev-lab/breadth/` — `10-dispatch.hs`, `11-dispatch-loop.hs` and
`32-traverse-content.hs`. `dispatch-tidepool.hs` here is a coherent invocation of
the same pattern against this repository, written rather than substituted. It has
not been executed, so its wording is deliberate and its numbers are unmeasured.

Read `.shoal/plans/README.md` for what is specific to working in this repository,
and load the `shoal-jev` skill for the question-writing rules.
