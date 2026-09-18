# Try these first

The cells worth running again, what each showed, and how to run them. Every one
of these executed against real artifacts and its numbers are in `NOTEBOOK.md`.

## Running any of them

Start a session against a workspace nothing else holds. The lock is per
workspace path, so two sessions need two paths.

```bash
cd ~/dev/tidepool-jev
export TIDEPOOL_DEV_FLAKE="git+file://$HOME/dev/tidepool-jev?rev=$(git rev-parse HEAD)"
TYPESAFE_API_KEY="$(< ~/.config/typesafe/api-key)" \
  bash scripts/dev-shell.sh --shoal scripts/shoal-init.sh \
    --workspace "$HOME/dev/shoal-evals/tui-test-app" \
    --session lab --no-attach --model gpt-5.6-luna --effort low

./target/debug/shoal proxy lab plans/jev-lab/breadth/10-dispatch.hs
```

Fixtures live in `plans/jev-lab/fixtures/`. Several cells read them from
`$CLAUDE_JOB_DIR/tmp/lab5/fx/`; copy the fixtures there or edit the one path at
the bottom of the cell. `trajectory.txt` is in the fixtures directory.

Read `RECOGNIZING-FIT.md` before writing a new one. It is short and it is the
part that took the longest to learn.

## The four to read first

**`10-dispatch.hs` and `11-dispatch-loop.hs` — a typed action dispatcher whose
alternatives carry real commands.** The headline of the survey. Three steps, no
model turn between them, fetching a commit message then a diff then every call
site. Shows the idiom for payloads that are `Eff` actions and for `J.handle`
dispatching into them. Also shows why a fetch loop stops repeating itself: each
alternative names what the state lacks, so satisfying it removes its own reason
to be chosen.

Known defect, left in place deliberately: it truncates each fetch to 700
characters before feeding it back into its own observations, which deletes the
deciding evidence. That is the bug to fix first if you develop this.

**`33-threeway.hs` beside `35-threeway-fair.hs` — the same program before and
after one wording repair.** Keep these two together; they are the worked example
of debugging a semantic program. The only difference is that in the second, all
four alternatives describe what the output contains. The ambiguous fixture goes
from wrong at mass 0.86 to right at mass and confidence 1.00.

**`32-traverse-content.hs` — navigation over a repository, with contents as
branch evidence.** Code lists children, the model picks one, code descends. Run
`30-traverse.hs` first to see the same program score branches by filename and
go to the wrong file at 0.51 against 0.47. Shows the `[bash| ... |]`
quasiquoter for fetching real content per branch.

**`37-reflect-intent.hs` — an agent's own history answering what the
diagnostics cannot.** Three separate named fields for instructions, history and
repository evidence, on a fully matched real fixture. Evidence alone refuses at
1.00; the history reaches the right answer at 0.83; off-task history returns to
`unresolved` rather than inventing something. The supersession cases at the
bottom are an open failure, recorded.

## The rest, by what they are good for

| cell | what it is good for |
|---|---|
| `23-recover-intent.hs` | comparing one question across several states, with the policy applied. The cleanest template for an ablation. |
| `22-discriminate.hs` | asking the same question twice under two premises with `J.given`, and ranking by the gap. Use when you want to know which read would settle a disagreement. |
| `36-trajectory.hs` | four independent conditions over a sequence of actions, with code deriving counters alongside. Also the template for a three-state ablation inside one cell. |
| `12-termination.hs` | four forms of one question in a single packet over identical state. The cheapest way to diagnose a question that is behaving oddly. |
| `52-question-self-lint.hs` | a packet that scores your own questions before you send them. One axis, "states the deciding fact", separated our measured-weak from measured-strong wordings 0.20 against 0.45. |
| `61-shadow-gate.hs` | running a shipped gate against labelled situations and sweeping the floor. Reports false accepts and false doubts separately, which is the only useful form. |
| `40-fanout-timing.hs` | wall-clock timing inside a cell, one packet against sequential calls. |
| `42-pool-size-drift.hs` | carrying known-answer anchors through several pool sizes. Copy the anchor discipline into any pooled cell. |
| `50-error-message-lint.hs` | pooling text we wrote for models to read and scoring it against a standard. Found 2 of 25 engine messages worth rewriting. |
| `60-commit-mapreduce.hs` | chunking hundreds of items into batches in Haskell, with fixed anchors in every batch. 300 items in 12 requests. |

## Cells that did not work, kept because the failure is the lesson

- `34-two-repairs.hs` asks counterfactuals ("would reverting fix this") and gets
  nothing on three fixtures. Ask what the state contains instead.
- `30-traverse.hs` scores branches by filename. Keep it beside
  `32-traverse-content.hs`.
- `51-skill-example-lint.hs` cannot separate a qualified library call from a
  local binding, so it flags everything. Ask per extracted name, not per block.

## Four rules that would have saved the most time

1. Every alternative in a choice describes the same kind of thing, and that
   thing is checkable against the state. Getting this wrong produces a
   confident wrong answer, not a weak one.
2. Rewrite **all** the alternatives or none. Changing two of three collapsed
   every confidence in that packet, including the answers that had been right.
3. Bind previews, not evidence. A cell's display budget is spent by what you
   send to the model, not by what you return, and a cell can succeed, pay for
   its calls, and show you six characters of its answer. `Cmd.quiet` helps.
4. Never truncate from the front. A check log begins with nix and cargo
   preamble and ends with the diagnostics.
