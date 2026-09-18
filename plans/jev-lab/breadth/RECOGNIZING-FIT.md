# Recognizing a problem that fits a cheap semantic judgment

Distilled from this survey's measurements and from a design discussion with the
supervising peer, 2026-09-17. The measured claims cite the experiment; the
framings are judgement and are marked as such.

The point of writing it down: we are building ordinary programs that contain
semantic operations. There is no universal shape for "a Jev program". What
follows is how to tell whether a particular question belongs to the model.

## The first test

**Does an authoritative representation already expose the answer, or does
answering require interpreting meaning?**

Not "could a careful reader see it". A careful reader can also see that two
passages contradict each other, and that still takes judgment.

| question | answered by |
|---|---|
| did the command succeed | exit status |
| which owned paths changed | git, plus the ownership rule |
| are these five diagnostics one cause | string comparison on the headline and the `defined here` target |
| does this test actually exercise cancellation | judgment, unless structured evidence already establishes it |
| which excerpt would best distinguish these two explanations | judgment |

Two of those were on my original list of things to ask about, and both were
wrong to ask. Grouping and ownership are string operations.

## The second test

**What behaviour changes because of the answer?** If no branch, ordering,
display or subsequent investigation consumes it, do not ask. This is the cheap
test and it retires most bad ideas before the first one.

## Know which of three things you asked for

The same interface returns all three and the program has to know which it got.

- **An established fact**, interpreted from evidence that is present.
- **A prediction** about something not yet observed, including someone's future
  reaction.
- **A useful perspective**, an editorial opinion with no fact of the matter.

A perspective is not worthless for lacking ground truth; it is worthless when
reported as a discovered property. Keep the original material visible beside it.

## Shapes that fit

**Simulating a reader, when you name the reader.** "Would a user notice this"
hides three questions: does observable behaviour change, would someone meet it
in ordinary use, and would that audience care. Name the audience and the
situation. Cheap simulated perspectives are good for deciding where to spend
real attention, and an occasional real interview corrects the perspective.

**Subjective questions, used to shortlist rather than to score.** Whether an
error message is kind has no correct answer, but whether a reader understood the
repair, felt blamed, or gave up is observable. Use such judgments to surface
names worth reconsidering or to compare two messages for one audience. Competing
perspectives, such as newcomer comprehension against experienced-user precision,
disagree usefully; a single quality score does not.

**Relational judgments.** Atomic means one coherent judgment, not only local
facts. Design coherence is relational: supply the related material together and
ask whether its parts support the stated invariant. Companion questions about
specific contradictions make that answer inspectable without replacing it.

**Trajectories, given events plus derived change.** A snapshot can contain a
history; the question is whether the representation preserves the changes that
matter. Supply a short ordered sequence of actions and outcomes, facts code can
derive (elapsed time, repeated commands, changing revisions), and the excerpts
saying what was tried. Neither a raw transcript nor counters alone will do:
five failed attempts may be productive if each eliminates a different
explanation, and five successful commands may accomplish nothing.

## Shapes that need care

**Absence needs a bounded search space.** These are different claims: this
supplied reply does not mention running tests; the inspected files contain no
implementation; the repository contains no implementation. Only the first has a
complete domain inside the packet. Let code record what was searched and what
was supplied, ask about meaning within that scope, and return "not found in
these materials" when that is what you established. Silence is not
contradiction, which is why the two are separate answers.

**Scale changes the acceptable cost of an error, not the tendency of errors to
cancel.** Ours did not cancel: a notes-only commit scored 0.85 on user
visibility, which is a systematic sensitivity to wording, and at scale that
favours an entire writing style. A rough question is still excellent for finding
twenty interesting entries among three hundred, and unfit for claiming a
percentage. Choose the question around its consumer: shortlisting tolerates
false positives, aggregate measurement needs consistent definitions and an eye
on selection bias, consequential routing cares about the single mistake.

Fixed anchors are necessary and not sufficient. Ours held within 0.09 across
twelve batches while a real error family sat in the results. Read a few high,
low and surprising entries yourself, looking for recurring families.

## Reasons to decline

- The required evidence is not available.
- An exact answer is required and an authoritative computation exists.
- The model is being asked to grant authority it does not possess. Adversarial
  material is still usable as evidence; what it cannot supply is authority.
  A classifier calling text harmless establishes no security boundary, and code
  still constrains the available actions, resources and effects.
- An error would cause unacceptable consequences with no adequate check or
  recovery.
- Nobody can say what behaviour the answer should change.

Note what is **not** on that list: holism, subjectivity, adversarial input, and
the absence of ground truth. None of those disqualifies a use on its own.

## Writing the question, once it fits

Measured in this survey and in the 600-call playbook that preceded it.

- Name the state field in backticks and state the deciding fact. Judgment
  questions sit near 0.4; literal ones separate. Saying *why* moved one question
  from 0.42 to 0.72.
- **Every alternative in a choice must describe the same kind of thing, and that
  thing must be checkable against the state.** An alternative describing a
  property of the repair space among three describing the output's contents took
  a routing decision from right to wrong at 0.86. Rewritten uniformly, the same
  fixture answered correctly at 1.00. Measured three times in this survey.
- Ask about what the state contains, never about a counterfactual outcome the
  state cannot witness. "Would reverting fix this" returned nothing on three
  fixtures.
- A branch is judged on the text you put in front of it. Filenames alone chose
  the wrong file at 0.51 against 0.47; the same choice with each file's contents
  chose right and dropped the loser to 0.18. Truncation silently deletes the
  deciding fact, and no answer tells you it happened.
- Every alternative set needs an exit written as a condition.
- Choice when one candidate must occupy one slot, and its masses are relative,
  so adding candidates moves them. Noul per item for independent conditions.
  Score for degree on an ordered rubric. Independent judgments for initial
  relevance, then a contextual choice if you need the single next useful read.

## Checking a semantic program

Nothing replaces the compiler; several checks cover different mistakes. Keep a
small set of saved runnable examples beside each reusable function: a normal
case, a missing-evidence case, a conflicting-evidence case, and the actual
failure that caused its last revision. Check the resulting **action**, not the
winning label or the exact probability. Test the ordinary code too: that
complete observations survive, that command failures stay failures, that every
alternative reaches its intended branch.

For a consequential packet, have another reader inspect the state, the
questions, the alternatives and the action mapping together. A wording-only lint
cannot discover that the deciding line was truncated. In this survey I shipped a
defect I had measured two experiments earlier and did not see it until asked to
audit.

The manageable unit is a saved runnable example, not a list of things the author
must remember.

## Finishing

A program may finish with an explicit unresolved result, and that is a third
outcome distinct from success and from budget exhaustion. "Strategy
established, four obligations found, coverage unresolved for two" is a useful
answer. Do not require everything to resolve before returning anything.
