# Ideas not run, and the ones I would pick up first

The survey is closed. This is what was on the list and did not get run, so that
a later wave does not re-derive it. `PLAN.md` holds the full original list with
each item's basis and fixture; this file records what happened to it and adds
what today's results suggest.

## The five I would run first

**1. A genuinely stuck worker, for the trajectory questions.** The one
trajectory we tested was productive, and there the derived counters added almost
nothing over the raw sequence. The counters should earn far more on a worker
that repeats a command with an unchanged exit code while its prose looks busy.
Until that is run, the claim that a trajectory needs both events and derived
change is only half tested. Fixture: run 7 has one, the `Project.Gate` start
rejected seven times across three actors with the same prepared-emitter error.

**2. Conjunct-count symmetry.** Supersession failed with an alternative set that
was uniform in vocabulary but not in shape: the winning alternative carried two
conditions and its rival carried one, and the winner's second condition was
strongly true. If matching the number of conditions fixes it, that is a new
constraint worth adding to the rules, and it is one cell.

**3. Close-tie double fetch.** When two candidate reads score close and both are
cheap, fetch both rather than treating the tie as a decision to make. A tie
between reads need not block anything. This is where breadth buys speed without
needing a sharper judgment.

**4. Score for reading order.** Score a handful of real fetched excerpts against
one unresolved question, on a rubric of unrelated, relevant background, narrows
the competing explanations, directly distinguishes them. Then read them in that
order and see whether the useful evidence comes first. We used Score almost
nowhere and this is its natural home: degree on an ordered rubric, rather than a
fact.

**5. Audience-specific interface critique.** Our error messages, skill text and
report formats, judged from named perspectives that disagree: a newcomer's
comprehension, an experienced user's precision, whether the repair is
actionable. The disagreement between perspectives is likely more useful than any
single quality score. `50-error-message-lint.hs` is the starting point and it
already found two messages worth rewriting.

## Also worth doing, lower priority

**Contextual tool-output selection.** Current task plus recent turns plus a tool
output, selecting the useful passages and keeping addresses for everything
omitted. Half of this exists as the adaptive view idea in `PLAN.md` and was
never run. The consumer interview already told us a count warns and does not
help, while an address list does both.

**Steering fan-out that actually forwards.** Run 7's root received operator
steering and forwarded none of it. Per live child, whether the steering changes
what that child's assignment asks, with the payload being a real message send.
This one acts on the world rather than reporting, which is why it is
interesting.

**Ownership gaps before launch.** Per requirement and per child, whether that
requirement can be implemented inside the paths that child owns. Would have
caught the run-7 contract gap before any child was admitted. Note that the
ownership check itself is prefix matching and belongs in code; the judgment is
only whether the work can be confined.

**Diff to requirement coverage.** Hunks against contract clauses, producing the
uncovered clauses and the unrelated hunks. The reviewer's "did it do everything,
and only that".

**Did the review add anything.** Run 7's reviews mostly restated the
implementer. Per reviewer sentence, whether it cites a line of the diff or test
output that the implementer's own report did not. A review below a floor is not
counted as a review.

**Rollout step classification.** Classify each step of a past run as reading,
editing, running the check, hand-rolling a loop, waiting or steering, and
compute the run-comparison numbers from a cell instead of from several analysis
agents. This improves our method rather than a run, which is why it kept
slipping.

**Lesson retrieval by meaning.** A new failure against the paragraphs of the
previous runs' notes, surfacing "we hit this in run 5" without embeddings or a
registry.

## Attempted and unresolved

**Autonomous termination.** Never demonstrated. The audit showed the loop was
asked to stop while holding evidence it had itself truncated away, so the low
answers were probably correct. The experiment to run is untruncated
observations with a completion condition that is true of the state once
reached. Given that routing works when the alternatives are uniform, I expect
this to be fixable.

**Position bias.** Measured zero spread, but the fixture was unambiguous and
every rotation saturated at mass 1.0, so there was no headroom. Rerun on a
genuinely close pair.

**Pruning satisfied alternatives.** Confounded three ways and unmeasured. The
hypothesis worth one clean experiment is that alternatives scoring near zero are
themselves evidence about the state.

**Skill example lint.** Flags everything because it cannot tell a qualified
library call from a local binding. Ask per extracted name against the shipped
list rather than per fenced block.

## Dropped

**Interpretations generated by a child model, then investigated.** The workbench
effect row carries no generation effect, so this needs a real child actor and a
turn of its time. Not worth a session on its own; fold it into a dogfood run if
it still looks interesting.

**Position in the survey for its own sake.** Several items in `PLAN.md` were
variations on patterns that had already answered their question, among them the
novelty check on progress notes, friction deduplication, and the handoff brief
to action list. They are cheap and none of them would change a decision we face.
