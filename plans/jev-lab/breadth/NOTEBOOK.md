# Breadth notebook: TypeSafe patterns and our recompositions

Live in Shoal session `lab5` against the toy repo, 2026-09-17. Every number
below is a raw distribution from a real call, not a summary. Cells are beside
this file. Outcomes are one of: **worked**, **interesting failure**, **needs a
missing capability**, **worth combining with**.

| # | pattern(s) drawn on | our composition | fixture | outcome | observation |
|---|---|---|---|---|---|
| A1 | function calling; payload is the continuation | menu alternatives carry real `git` commands; three steps, observations threaded back | `53ad43c` failed check + the store leaf's assignment | worked, with two sub-failures | fetched exactly the right evidence chain in three steps and never repeated itself; the exit alternative never fired and the speculative argument question gave no signal |

---

## A1. Typed action dispatcher against changing state

**Documented**: the function-calling cookbook bundles a routing choice and its
branches' arguments in one request and reads only the branch taken. The
`shoal-jev` skill's "the payload is the continuation" says alternatives carry
the thing that runs.

**Ours**: the payloads are `Eff` actions that shell out to `git` in the toy
repo, and the loop runs three steps with no model turn between them, appending
each fetched text to an `observations` field that the next question reads.

Alternatives, each phrased as a condition on what `observations` lacks:
read the commit message, show the definition's diff, grep the tree for call
sites, read the written requirements, plus the exit "`observations` already
says whether the change at the definition was deliberate".

### What it fetched, in order

| step | key | mass | margin | confidence |
|---|---|---|---|---|
| 1 | `read_commit_message` | 0.59 | 0.27 | 0.49 |
| 2 | `show_definition` | 0.37 | 0.08 | 0.22 |
| 3 | `grep_callers` | 0.41 | 0.02 | 0.26 |

Step 1 returned "Cap the number of items load returns". Step 2 returned the
hunk adding `limit: usize` and `.take(limit)`. Step 3 returned all five call
sites including `src/main.rs:83`, which is outside the leaf's owned paths.

**The chain is exactly right, and no model turn chose it.** This is the
evidence Sol had to be told by a human in the earlier interview.

### It does not repeat itself, and that is the wording's doing

`read_commit_message` fell 0.59 → 0.29 → 0.05 as its text entered
`observations`. Each alternative names the thing `observations` does not yet
contain, so satisfying it removes its own reason to be chosen. Phrasing an
alternative as a condition on state, not as an action, is what makes a fetch
loop terminate on its own.

### Sub-failure 1: the exit never fired

`#settled`, asked each step as "Does `observations` already state whether the
parameter added at the definition was added on purpose?", answered 0.04, 0.06,
0.08 — flat, and wrong by step 2, where `observations` held both a commit
message saying the cap was the point and a diff using the parameter. The exit
alternative in the choice sat at 0.01 throughout.

An alternative that means "stop" competes badly against four that each name a
concrete missing thing: the missing things are easy to check and stopping is
not. The loop as written would fetch until it ran out of menu.

### Sub-failure 2: the speculative argument question was dead

`#term`, asked under `J.given "the next observation is a tree-wide search for
call sites"` as "Is `load` the right term to search for, rather than the name
of the function containing the first reported call site?", returned 0.52, 0.53,
0.54 across three steps. No signal at any step, including the step that
actually ran the search. The premise was fine; the question asked for a
comparative judgment rather than a fact in the state.

### Cognition absorbed

The "what do I read next" turn, three times over. An agent doing this by hand
spends one turn per read plus the reading itself. Here it is one cell and three
Jev calls, and only the 700-character tails reach a context window.

### What would change next

Ask the stop condition as a Noul against named content, not as an alternative
in the same choice, and test that directly. Rewrite `#term` as a fact question.

---

## B2. Searching for the observation that separates two explanations

**Documented**: speculative fan-out asks conditional questions before knowing
which branch applies, under a premise with `J.given`.

**Ours**: the premises are two competing explanations of one failure, and the
same concrete question is asked once under each. The gap between the two
answers is the observation's discriminating power, computed in code. Nothing
in the cookbooks asks a question twice to measure how much its answer would
move a belief.

Fixture: the `53ad43c` failed check. Explanations: the parameter was added on
purpose and the callers are stale, versus it was not meant and the callers are
right.

| candidate observation | if deliberate | if accidental | separates by |
|---|---|---|---|
| read the commit message | 0.68 | 0.46 | 0.22 |
| show the definition's diff | 0.67 | 0.18 | **0.49** |
| grep the tree for call sites | 0.12 | 0.59 | 0.47 |
| read the written requirements | 0.69 | 0.41 | 0.28 |

The program picked the definition's diff, and that is the read that in fact
carries the decisive evidence: the body uses the new parameter (`.take(limit)`),
which an accidental parameter would not. The call-site question inverts rather
than separating in the same direction, which is also informative: the model
reads "deliberate" as implying the author would have updated the callers, so
their being stale is surprising under that explanation.

**Outcome: worked.** Two concrete questions per candidate are enough to rank
reads by how much they would settle a disagreement.

---

## B2b. Does self-fetched evidence recover what a human had to supply?

**The question yesterday's interview left open.** The investigation disagreed
with Sol only because it lacked the assignment. So: can the program fetch its
own way to that fact? The same choice, asked three ways, nothing else changed.

| state | winning key | mass | margin | confidence | accepted under `J.merging` |
|---|---|---|---|---|---|
| nothing | `insufficient_evidence` | 1.00 | 1.00 | 0.99 | `insufficient_evidence` |
| 1519 bytes the program fetched itself | `callers_catch_up` | 0.75 | 0.60 | 0.63 | **doubt: Unconfident 0.63** |
| one sentence a human supplied | `callers_catch_up` | 0.97 | 0.95 | 0.94 | `callers_catch_up` |

**This is the sharpest result of the session.** Self-fetched evidence recovers
the right answer — the same key Sol reached on its own — but not enough
confidence to act on it. The policy holds it back.

Gathering evidence converts a flat refusal into a correct but unconfident
answer. Stating the intent converts it into an actionable one. They are not
substitutes, and the gap is large: 0.75 against 0.97 on mass, 0.63 against 0.94
on confidence, from a commit message, a diff and a call-site list against one
sentence.

**Outcome: worked, and it sharpens the earlier lesson rather than overturning
it.** A program that fetches its own evidence should report a ranked answer
with its doubt, and should still ask for the assignment when one is available.

---

## A1b. Why the dispatcher never stopped (Astra's follow-up)

One request, one state that already contains a commit message, the definition's
diff and the call-site list. Four forms of the same stop question, so nothing
but the form differs.

The original completion alternative was written as a belief ("`observations`
already says whether the change was deliberate") while every fetch alternative
was written as an artifact ("`observations` contains no commit message"). The
follow-up gives completion the same artifact vocabulary.

| form | completion mass | winner | confidence |
|---|---|---|---|
| original wording, in the earlier loop | 0.01 | a fetch | 0.22-0.49 |
| concrete wording, full menu | **0.39** | `read_requirements` 0.45 | 0.31 |
| concrete wording, menu pruned by code to the one unsatisfied fetch | **0.15** | `read_requirements` 0.85 | 0.69 |
| the same concrete question asked as its own separate noul | 0.21 yes | — | — |
| the original vague question as its own noul | 0.36 yes | — | — |

**Observed.** Giving completion the artifact vocabulary moved it from 0.01 to
0.39, from ignored to a near tie for the win. Pruning the satisfied actions,
which looks like the obvious code-side optimisation, moved completion the wrong
way, from 0.39 down to 0.15. Asking the same concrete question as a separate
noul scored 0.21, below the in-choice alternative.

**A caution about my own question.** The concrete wording asks for a commit
message that "names the new parameter". The real commit message is "Cap the
number of items load returns", which names the behaviour and not the parameter,
so the conjunction is literally false. The 0.21 separate answer may be correct
rather than a failure, and the in-choice 0.39 may be held down by the same
false conjunct.

**Unresolved.** Whether completion would win outright with a conjunct that is
actually true of the state. Whether the pruning effect is that satisfied
alternatives scoring near zero are themselves evidence of completeness, which
is a testable claim and not yet tested.

**Not shown.** Autonomous termination. No form tried here produced a run in
which the loop stopped itself.

**What this does settle.** Mixed vocabulary inside one choice is a real defect:
alternatives that describe artifacts and an alternative that describes a belief
do not compete on equal terms. And a separate question is not automatically the
repair — here it was worse than fixing the alternative in place.

---

## B7. Semantic traversal of a repository, and what fixed it

**Documented**: the emergent web-traversal pattern ("Jev Browser") puts the
model in a navigation loop over DOM elements, selecting links without
generating text, so it cannot invent a dead end.

**Ours**: the tree is a git repository rather than a DOM. Code lists the
children of the current node, Jev scores each, code descends into the winner
and lists its children. Nothing generates a path, so no step can name a file
that does not exist. The goal was stated once: "the code that runs when the
user presses the f key to cycle the item filter". Ground truth, which the
program was never told: `src/panels/list.rs:88`.

### Run 1, each branch described only by its name

| hop | chose | score | runner-up |
|---|---|---|---|
| root | `src` | 0.87 | 0.37 |
| `src` | `src/app.rs` | 0.51 | 0.47 |
| `src/app.rs` | `impl App {` at line 71 | 0.68 | 0.22 |

Three hops, 1343 ms, and **wrong**. The `f` key is handled in
`src/panels/list.rs`, not in `app.rs`.

### The diagnosis

The traversal was scoring names, not contents. `app.rs` is a name that plainly
relates to a filter, because the `Filter` type does live there. `panels` is an
opaque directory name carrying no evidence whatsoever. A web-traversal loop
gets anchor text with every link for free; a directory listing has none.

### Run 2, one variable changed: each branch carries real text

Each child is now described by content fetched with a shell pipeline: a
directory by the function, `impl` and `KeyCode::` lines of the Rust files
inside it, a file by its own first forty lines.

| hop | chose | score | the rest |
|---|---|---|---|
| root | `src` | 0.63 | TASKS.md 0.38, .agents 0.15, .shoal 0.15, README 0.14 |
| `src` | `src/panels` | **0.56** | main.rs 0.30, **app.rs 0.18**, panel.rs 0.16, store.rs 0.03 |
| `src/panels` | **`src/panels/list.rs`** | 0.52 | help.rs 0.22, mod.rs 0.14, status.rs 0.10, detail.rs 0.06 |

**Right answer, and the near-tie is gone.** `app.rs`, which won the first run
at 0.51, fell to 0.18 once the model could see that its contents are a struct
and a visibility filter rather than key handling. 4685 ms for three hops,
against 1343 ms for the name-only version: the content costs about 3.5x the
wall clock and buys the correct destination.

**Outcome: worked, after a failure that taught the rule.** The transferable
rule is that a branch must carry evidence, not a label. Where an alternative
names a thing the model cannot see inside, spend a cheap shell command to put
its contents in the description. This applies to every choice we write over
files, modules, actors or skills, and it is the same defect as the earlier
finding that mixed vocabulary breaks a choice: in both cases an alternative
was being judged on something other than the evidence.

**Not shown.** Whether a beam that keeps two paths alive would have rescued
the name-only run. Adding evidence fixed this case outright, so the beam is
untested here.

---

## B8. Three-way branching, and the case it fails to catch

**Documented**: threshold-based autonomy. Every answer carries a calibrated
confidence, so high confidence acts, moderate confidence escalates to an
expensive generative model, and low confidence goes to a person.

**Ours**: the three destinations are three real read-only programs rather than
labels. `mechanical` greps the tree for the lint's site list; `needs_writing`
fetches the enclosing code as a brief for a model; `needs_intent` reads the
commit subject and composes the question to put to a person.

Three real failed checks whose correct destination I determined independently
before running anything.

| fixture | what it really is | chosen key | mass | confidence | correct? |
|---|---|---|---|---|---|
| `4610b5e` | a clippy lint promoted by `-D warnings` | `mechanical` | 0.95 | 0.93 | yes |
| `f726882` | five non-exhaustive match arms | `needs_writing` | 0.98 | 0.97 | yes |
| `53ad43c` | wrong arity, two coherent repairs | `mechanical` | 0.86 | **0.81** | **no** |

**The first two are right and confident. The third is wrong and confident.**

`53ad43c` is the fixture where updating the callers and restoring the signature
are both coherent repairs, and where a human had to supply the intent before
the earlier investigation would choose. Routing called it mechanical at 0.86
mass and 0.81 confidence, which under any of our three named policies would
have acted on its own.

**This is the failure that matters.** A confidence floor protects against
evidence that is missing or contradictory. It does not protect against
ambiguity that is invisible in the evidence. Nothing in the text of four
`E0061` diagnostics indicates that two repairs compete; the diagnostics are
perfectly clear, and a caller passing one argument to a two-argument function
is, on its face, mechanical. The model is not miscalibrated about what it can
see. The thing that makes this case need a person is not in what it can see.

So the three-way pattern as published cannot be used to decide when to ask a
human on our workload. The escalation to a person has to be triggered by a
structural fact that code can check, and we already know what that fact is
from the earlier work: the failure admits more than one whole repair. That is
a question you ask separately and explicitly, as the strategy choice does, not
something you read off a confidence score.

**A defect in my own harness, recorded so the table is not misread.** I labelled
the tiers by confidence and dispatched by key in the same cell, so the tier
string printed for `f726882` reads "code acts on its own" while the branch that
actually ran was the model brief. The branch behaviour was correct; the tier
label was a second, redundant computation and should not have been there.

**Outcome: interesting failure**, and the most useful one of the session.

---

## Worker results, run in parallel sessions

Three Sonnet workers ran their own Shoal sessions against private clones of
the toy repository. Their full notes are in `NOTES-lab6.md`, `NOTES-lab7.md`
and `NOTES-lab8.md` beside this file; cells are `40-` through `61-`.

### Calibration (lab6)

| experiment | result |
|---|---|
| speculative fan-out | 13 questions in one packet 317 ms; the same 13 sequentially 2213 ms; **6.98x**, against a published claim of 10.0x. Answers agreed between the two runs, so the speed was not bought with worse answers. |
| position bias | no signal, and honestly reported as such: the fixture was unambiguous and every rotation landed the same key at mass 1.0, leaving no headroom for position to move. Not evidence that position bias is absent. |
| pool size drift | two ground-truth anchors held at 0.82/0.79/0.81 and 0.04/0.04/0.04 across pools of 5, 15 and 30. **Max spread 0.03.** No batch-size effect in that range. |
| description length | the choice half saturated at 1.0 both ways, no signal. The pool half moved a lot: raising a preview from 200 to 1200 characters moved `.yes` from 0.58 to 0.90 and 0.11 to 0.36, **+0.25 to +0.32**, because the deciding sentence sat just past the 200-character cut. |

That last row is an independent confirmation of the traversal finding above,
reached from a different direction: what a branch is judged on is whatever text
you actually put in front of it, and truncation silently removes the deciding
fact.

### Jev linting our own tooling (lab7)

| experiment | result |
|---|---|
| engine error messages | worked. The canary, our best refusal message, scored 0.79/0.81/0.96 on the three axes, so the packet was live. **2 of 25** real engine messages scored below 0.5 on all three axes and are genuine rewrite candidates. |
| skill example lint | interesting failure. The "uses an unshipped name" question never fell below 0.68 on any block, including the two that use only shipped verbs, because it cannot tell a qualified library call from an ordinary local binding. The question as posed cannot separate the cases. |
| a packet that lints its own questions | worked, on two axes of three. Measured-strong question wordings averaged **0.45** on "states the deciding fact" against **0.20** for measured-weak ones. It independently flagged both of the live failures from my own runs: the dead comparison question at 0.19 and the belief-shaped completion question at 0.41. The backtick-naming axis showed no separation and should be dropped. |

The third row is the most reusable: a packet can screen its own questions
before they are sent, using one axis, at the cost of one extra request.

---

## B8b. The escalation trigger I proposed does not work either

After routing was confidently wrong on the ambiguous failure, the obvious
repair was to ask the structural fact directly rather than hope confidence
would reveal it. Two literal questions per failure, neither of which asks
whether the case is ambiguous; code was to call for a person when both whole
repairs scored above a half. This is atomic decomposition, and it is the shape
the published guidance recommends.

Questions, over the diagnostic lines of each failed build:

- "Would putting back the code at the location these diagnostics name as the
  definition make every reported error go away, without editing any of the
  reported sites?"
- "Would editing every site these diagnostics report make every error go away,
  without changing the code at the location they name as the definition?"

| fixture | revert would fix | editing the sites would fix |
|---|---|---|
| `4610b5e` lint | 0.42 | 0.64 |
| `f726882` missing arms | 0.42 | 0.63 |
| `53ad43c` **the ambiguous one** | **0.22** | 0.66 |

**No separation, and the ambiguous case scored lowest of the three on the
revert question**, which is the opposite of what the test needed.

**Why, and this is the useful part.** Reverting is only a visible repair if you
know what the definition changed *from*, and a diagnostic never says that. The
state carried diagnostic lines only. The model was answering about a
counterfactual it had no evidence for, and it answered low for all three, which
is reasonable.

This closes a loop across three experiments in this notebook. B2b asked the
strategy question with the commit message, the definition's diff and the call
sites in hand, and got the right answer at 0.75. B8 asked for a routing
decision from diagnostics alone and was confidently wrong. B8b asked the
structural question from diagnostics alone and got nothing.

**The conclusion is about evidence, not about wording or thresholds.** Whether
a failure admits two repairs is not a property of its diagnostics. It is a
property of the change that caused it. A program that wants to know when to
stop and ask a person has to fetch the diff first, and once it has the diff it
can ask the strategy question directly and does not need a separate trigger at
all. The dispatcher in A1 already fetches exactly that. The pieces compose;
routing on diagnostics alone was the mistake.

**Outcome: interesting failure**, and it retires the idea of a cheap
diagnostics-only escalation trigger.

---

## B8c. The routing failure was my wording. Retract B8 and B8b.

Asked whether the earlier negatives proved anything about the pattern or only
about my execution, I re-read my own alternatives and found the defect I had
already measured two experiments earlier.

The original four alternatives:

- `mechanical`: "`check_output` names a lint rule or shows a formatting diff, and the repair it asks for is fully determined by the diagnostic text."
- `needs_writing`: "`check_output` reports missing code that has to be written..."
- `needs_intent`: "**`check_output` reports a conflict that two different repairs would both resolve**, and the diagnostic text does not say which was meant."
- `unreadable`: "`check_output` contains no compiler diagnostic naming a file and a line."

Three describe what the output contains. The third describes a property of the
repair space, which is not a thing a reader can check against the text. That is
exactly the mixed-vocabulary defect from A1b, where the odd alternative sat at
0.01.

Rewritten so all four describe contents, `needs_intent` becoming "`check_output`
names one definition and several sites that call it, and reports that the two
disagree". Nothing else changed: same fixtures, same state, same policy.

| fixture | original wording | uniform wording |
|---|---|---|
| `4610b5e` lint | `mechanical` m 0.95 c 0.93 — right | `mechanical` m 0.96 c 0.95 — right |
| `f726882` missing arms | `needs_writing` m 0.98 c 0.97 — right | `needs_writing` m 0.86 c 0.80 — right |
| `53ad43c` two repairs compete | `mechanical` m 0.86 c 0.81 — **wrong** | `needs_intent` **m 1.00 c 1.00** — right |

**Three-way branching works on our workload.** The case that needed a person is
identified at mass 1.00 and confidence 1.00, from the diagnostics alone.

### What this retracts

**B8's conclusion is withdrawn.** I wrote that a confidence floor cannot detect
ambiguity absent from the evidence. The ambiguity was in the evidence and the
alternative describing it was unaskable as written.

**B8b's conclusion is withdrawn too.** I wrote that whether a failure admits two
repairs is a property of the change rather than of its diagnostics, and that a
program must fetch the diff before it can know to ask a person. The uniform
wording answers it from diagnostics alone at 1.00. B8b's own two questions were
counterfactual ("would reverting fix this?") rather than about what the text
contains, which is a second form of the same error: asking about an outcome the
state cannot witness instead of a fact the state carries.

### What survives, and is now the strongest finding in the survey

**Every alternative in a choice must describe the same kind of thing, and that
thing must be something a reader can check against the state.** Measured three
times now:

| case | odd alternative | uniform alternative |
|---|---|---|
| completion inside a fetch menu | 0.01 | 0.39 |
| the ambiguous failure, routed | wrong at 0.86 | right at 1.00 |
| our own question wordings, linted | 0.20 | 0.45 |

The cost of getting this wrong is not a weak signal that you notice. It is a
confident wrong answer that clears every policy floor.

**Outcome: worked**, and it converts two earlier interesting failures into
evidence for one rule.

---

## Audit of the positives (Astra's request)

Two claims in this notebook overstated what the runs showed.

### The shadow test's "false accept" was a misrouted non-merge, not an approval

Situation S6 is a candidate carrying no diff at all. Ground truth said the gate
should answer `insufficient_evidence`, meaning the packet lacks what it needs.
It answered `item_missing` at mass 0.91, margin 0.82, confidence 0.88, clearing
every floor of `merging`.

Calling that a false accept overstates the harm, and our own skill file says so
plainly: `Right` from `J.accept` means the winning key cleared the floors, never
that the candidate was approved, and you dispatch on the key. Both keys are
non-merge outcomes. **The candidate is not merged under either answer.**

What the error actually costs: `item_missing` sends a repair request to the
child, while `insufficient_evidence` sends the parent to fetch the diff. So the
real consequence is a wasted round trip and a child blamed for an artifact the
parent failed to collect. That is worth fixing and is not a merge of bad code.

The finding that survives unchanged: the tripwire noul read 0.48 against 0.87
to 0.93 elsewhere, and the gate ignored it. That signal was present and unused.

### The dispatcher fed its own truncations back into its state

`dispatch` returned `T.take 700` of whatever it fetched, and the loop appended
exactly that string to `observations`. Step two fetched the definition's diff,
already cut to 1200 characters, and the 700-character record of it ends
mid-line at `-pub fn load(path: impl AsR`.

**So the decisive evidence never entered the state.** The line adding
`limit: usize` and the line using it in the body were both past the cut. Step
three was asked whether `observations` settled the author's purpose while
holding a diff that stopped before the change.

This reverses my reading of A1's first sub-failure. I recorded the completion
option staying at 0.01 to 0.08 as a wording defect. It is at least as likely
that **the answers were correct**: the state did not contain what the
completion option described, because my own code had removed it.

The same applies to A1b. Its state was built from a fresh fetch cut at 900
characters, which reaches the line adding the parameter but probably not the
line using it, and the option's first conjunct, a commit message naming the new
parameter, is false of a message that reads "Cap the number of items load
returns". Both conjuncts were unsupported. A 0.21 answer to a conjunction whose
parts are not in the state is a correct answer.

**Revised verdict on autonomous termination: not yet tested.** The runs so far
asked a completion question about evidence the program had truncated away. The
experiment to run is the loop with untruncated observations and a completion
condition that is true of the state when it is reached.

This is finding 4 of the survey happening inside the survey's own first
experiment, which is the most pointed illustration of it available.

### Scope of the wording conclusion

Uniform, content-shaped alternatives fixed **this** routing failure on **these
three fixtures**. That is a repair demonstrated on one program, not a general
law about Jev. Missing evidence remains a separate and independent failure
mode, and the audit above shows it was operating in this very notebook at the
same time. Two distinct causes were in play, and fixing one does not retire the
other.
