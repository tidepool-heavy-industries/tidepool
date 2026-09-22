# Reading a failed check with `Project.Investigate`

One call turns the literal output of a failed `./check.sh` into the list of
places that must change, the places that must not, the related tests, and
whether the work reaches outside the paths you own. It runs its own `git`
reads, so you do not spend turns narrating a sequence of file opens.

## The call

```haskell
import qualified Project.Investigate as Inv

Inv.renderInvestigation <$> Inv.investigate
  Inv.defaultInvestigationPolicy
  "/home/inanna/dev/exomonad-evals/tui-test-app"   -- repository to run git in
  "f726882"                                     -- the revision the check ran on
  ["src/panels/"]                               -- the paths you own
  ["adding a tag from the detail panel updates the visible tag list"]
                                                -- contract clauses a test would assert
  ["the commit deliberately adds a limit parameter to store::load"]
                                                -- what the change was for; see below
  "bash ./check.sh"                             -- the command that failed
  101                                           -- its exit status
  checkOutput                                   -- its literal output
```

Keep the `Investigation` itself if you want the numbers; `invJudgments` holds
every location's raw answers beside the routed lists, so you can apply your own
floors instead of the policy's. Note that binding the whole record in a
notebook cell can exhaust the turn's observation budget, because the excerpts
are large; render it, or read one field, rather than binding it whole.

## A worked result

On `f726882`, whose check fails with five `error[E0004]: non-exhaustive
patterns: app::ActivePanel::Tags not covered`:

```
bash ./check.sh exited 101; the diagnostics are about app::ActivePanel::Tags

must be edited:
  - src/panels/status.rs:29  (reported by the compiler in G1)
  - src/panels/status.rs:49  (reported by the compiler in G1)
  - src/main.rs:151  (reported by the compiler in G1)
  - src/main.rs:158  (reported by the compiler in G1)
  - src/main.rs:136  (reported by the compiler in G1)

leave alone:
  - src/app.rs:61  (note: `app::ActivePanel` defined here)

related test locations:
  - src/app.rs:293  (exercises the code but asserts no listed requirement)
  - src/app.rs:294  (exercises the code but asserts no listed requirement)

requires an ownership decision, which this investigation does not make:
  repairing app::ActivePanel::Tags requires edits in files outside the owned
  paths; another worker's assumptions may depend on them
  proposed addition to the owned paths: src/main.rs
  - src/main.rs:151  (reported by the compiler in G1)
  - src/main.rs:158  (reported by the compiler in G1)
  - src/main.rs:136  (reported by the compiler in G1)

the compiler's suggested fix inserts a placeholder; do not apply it as written

coverage:
  - 8 of 8 candidate locations examined
  - 2 test bodies read
  - searched for the qualified name ActivePanel::Tags
  - a broader search for Tags matches 3 lines; only the 2 matching
    ActivePanel::Tags were examined
```

## Supply the intent

The `intent` argument is the one input that changes the shape of the answer, and
it is the one thing the diagnostics cannot supply. A failure like a changed
signature admits two whole repairs: bring the callers up to date, or put the
signature back. Given nothing, the report names both and chooses neither, which
is the honest answer but not a useful one. Given one sentence saying what the
change was for, it commits, and every obligation follows mechanically.

Put your assignment there, in your own words. A sentence is enough.

## How to read it

**It diagnoses, it does not decide.** An edit outside your owned paths comes
back as a proposed ownership expansion with the sites and the reason for each.
It is not permission to make the edit. Reply to your parent with the proposal
and let them authorise it, revise the contract, or give the work to whoever
owns those files. A mechanical consequence of your own change is still a change
another worker may be assuming did not happen.

**Coverage is part of the answer.** A budget can stop the search, and when it
does the report says how many candidates it did not examine and what a broader
search would have matched. Do not read a short `must be edited` list as proof
there is nothing else; read the coverage lines.

**`related test locations` means related, not sufficient.** A location is
listed there because it is test code that touches the same thing. Whether it
asserts anything you were asked to deliver is the separate note in parentheses,
and it is only present when you passed contract clauses in. "Exercises the code
but asserts no listed requirement" means the test will not catch a regression
in what you were asked for.

**Probabilities, not verdicts.** Each routed list comes from a floor applied to
a probability. `invJudgments` keeps the numbers. A location just under a floor
is not a location the model said no to.

## What it costs

Two model requests, or three when related tests are found and contract clauses
were supplied. Roughly one `git grep` and one `git show` per file involved.

## When it will not help

A failure whose output carries no `path:line:col` diagnostics at all: a linker
error, a test harness that crashed, a timeout. `splitDiagnostics` will find
nothing and the report will say so in its coverage. Read the raw output
yourself in that case.
