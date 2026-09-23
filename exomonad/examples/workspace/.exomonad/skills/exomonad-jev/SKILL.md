---
name: exomonad-jev
description: Ask Jev, the cheap judgment model, inside a Haskell cell — packets, per-item batteries, calibrated alternatives, and gating an action on a policy. Load when a cell needs a semantic decision (triage, routing, relevance, a review gate) instead of another model round.
---

Jev returns typed semantic judgments inside an effectful program. Batch independent
questions over shared evidence; sequence calls when new evidence changes the
question. Code owns authoritative facts and prepared actions. A judgment supplies
evidence, not authority. Measure latency and usage for the actual workload.

Everything named in this skill is **shipped**: `J.ask`, `J.ask1`, `J.askWith`,
`J.choice`, `J.noul`, `J.score`, `J.each`, `J.optional`, `J.alt`, `J..|`,
`J.many`, `J.level`, `J.state`, `J.rawState`, `J.field`, `J.settle`,
`J.takenUnder`, `J.judge`, `J.holds`, `J.grade`, `J.graded`, `J.handle`,
`J.taken`, `J.contenders`, `J.explain`, `J.massAtOrAbove`, `J.answers`,
`J.resolvedModel`, `J.lenient`, `J.careful`, `J.strict`. The one exception is
the **effect type** `Jev`, which a record actor's row must name and which is not
re-exported by the workbench surface: `import Tidepool.Effects.Core (Jev)`.
Nothing from `exomonad/examples/workspace/.exomonad/Project` appears here.

`import qualified Jev.Operators as J` is in scope; `:=` and `:&` read
unqualified.

`J.ask1 state question` asks one; `J.ask state packet` asks a whole packet in
one round trip. Both return `Either J.JevError _`. Read the error and fall back
to your own policy; never retry blindly.

## Be dense

One packet per semantic boundary. Gather evidence, then batch the questions it
can answer. Use previews only when they contain the deciding evidence; otherwise
read the complete source. The example below stops on failed reads. Ask again when
new evidence or a changed question warrants it.

```haskell
listed <- Cmd.stdout <$> Cmd.run [bash|ls|]
let names = either (error . T.pack . show) T.lines listed
previews <- forM names $ \n ->
  (n,) . either (error . T.pack . show) id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [n] [bash|head -n 20 -- "$1"|])
```

A packet is a chain of labelled cells joined with `:&`. Nothing terminates it,
and two packets join the same way, so a shared set of questions is an ordinary
value: define one in a module, append it to the questions this call needs, and
the answer carries both sets' fields. A label written twice is a compile error.

`:&` is not `<>`, though. Each append changes the packet's type, so packets
cannot be collected in a list and folded, and there is no empty packet to start
from. A set of questions that varies at run time is therefore not a list of
packets; it is a list of your own values, which composes with `<>` as any list
does, turned into one packet by `J.each`. The candidate set is an ordinary
Haskell list too: `J.each key question rows` asks one question per row and hands
each row back beside its own answer, so there is nothing to look up afterwards.
A fixed section and a per-row section belong in the same packet:

```haskell
#budget := J.noul "Is this past the effort the assignment justifies?"
  :& #checks := J.each checkName (\c -> J.noul (checkQuestion c)) checks
```

```haskell
let packet =
      #enough := J.noul "Is a 20-line preview enough to judge each file, or does judging need the whole file?"
        :& #worth_reading := J.each fst (\(n, p) -> #keep := J.noul ("Worth reading " <> n <> " in full for this review? Its first 20 lines are:\n" <> p)) previews
answer <- J.ask (J.state (#task := ("triage files before a focused review" :: Text))) packet
fmap (\r -> (r.enough.yes, [(n, s.keep.yes) | ((n, _), s) <- r.worth_reading])) answer
```

`J.state` takes a field packet written exactly as a question packet is, each
field keeping its own Haskell type — `Text`, `Bool`, `Int`, `Double`, a list, a
list of `(Text, a)` for an object, a `Maybe`, or a nested field packet.
`J.field #task world` renders a checked reference to a field in wording, so a
question naming the state cannot drift from it; a name the state lacks is a
compile error listing the ones it has. `J.rawState someValue` sends a `Value` as
given, for a shape the field packet leaves out; its fields cannot be named.

Use previews when they contain the evidence the question needs; label their scope
and retain paths for expansion. Keep complete values or recoverable references.
Use Cmd.quiet and small display projections to avoid flooding the conversation;
do not discard deciding evidence merely to shorten its display.

## Reading the answer

The answer has the packet's shape. A cell labelled `#enough := J.noul …` is read
as `a.enough.yes`, a likelihood from 0 to 1. A cell labelled
`#per_file := J.each key question rows` is read as `a.per_file`, a list of
`(row, answer)` pairs in row order, where `row` is the value you passed in, not
its key, and `answer` has the shape of what `question` built: with
`\row -> #on_path := J.noul … :& #enough := J.noul …` each answer is read as
`ans.on_path.yes` and `ans.enough.yes`. So
`[(name, ans.on_path.yes) | ((name, _), ans) <- a.per_file]` projects one
likelihood per row.

Answers are data, read with record dot straight off the response — `r.enough`,
`r.worth_reading` — with no projection first; `J.answers r` hands the whole
packet to a function that wants it as one value. A choice answer has `.key`,
`.mass`, `.margin`, `.confidence` and `.masses`; a noul has `.yes`; a score has
`.expectation`, `.confidence` and `.masses`. Those fields are all there is: an
answer cannot be built or matched, and what it decides is reached through
`J.settle`, `J.judge`, `J.grade`, `J.taken` and `J.takenUnder`. Ending a cell
with the bound `answer` shows every distribution — do that the first few times
you write a packet, then project what the next decision needs.

`.key` is for logs and journals, never for dispatch: a `case` on it is unchecked,
and the compiler cannot tell you when the alternatives change. `.margin` is the
winner's mass less the runner-up's, and equals the mass when nothing competes,
so a margin near 1.0 usually means the alternatives were not really rivals.
`J.contenders floor answer handlers` reads every alternative at or above a mass
floor, best first, each already through the same handlers; a near tie is a typed
outcome worth branching on. `J.handle answer handlers` eliminates a choice
exhaustively with no policy at all, for when the program follows the winner
whatever it is.

A policy is three floors — mass, margin, confidence — named for how bad it is
to be wrong: `J.lenient` for read-only choices (which file, which skill),
`J.careful` for starting a worker or choosing an approach, `J.strict` for
merging, stopping, anything that leaves a receipt. `J.settle policy answer
handlers` returns `Either J.Doubt (J.Settled p r)`: `Right (J.Settled result)`
through the handler for the alternative that won, or `Left` a
`J.Doubt { cause, why }` whose `cause` is `NearTie`, `Underweight` or
`Unconfident` and whose `why` is the line a log reads. `J.takenUnder` is the
same with no handler list when every alternative carries the same type of
payload, `J.judge` and `J.holds` do it for a noul — a doubt is not a no, and
both read as `False` under `J.holds`, which is why it is a separate verb — and
the verdict carries the policy that reached it, so a function that must not be
handed a lightly-settled answer can demand `J.Settled J.Strict Patch` in its
own signature.

`Right` means the **winning alternative cleared the floors**, whatever that
alternative is: a confident `insufficient_evidence` is a `Right`. Never treat
`Right _` as approval; the handler that ran is what says what happens. A
selected answer still depends on the supplied evidence. Choose review and
escalation from the task's policy and consequences. A known-answer question can
help diagnose a packet; passing it does not establish that every other answer is
correct.

## Write questions against the evidence

- Include task intent when it changes the answer. The same diagnostic can require
  updating callers or restoring a signature depending on the assignment.
- Describe comparable conditions across alternatives. Include a described exit
  for missing evidence or no applicable option.
- Choice compares alternatives; Noul asks whether a condition holds; Score
  describes degree. Use independent questions when several conditions can hold.
- The named policies are convenient starting points. Test their behavior on your
  actual decisions. Confidence is not proof of evidence coverage or correctness.
  A mass of 1.0 means no offered alternative competes, including when the state
  or alternatives are inadequate.
- Preserve artifacts and distinguish them from reports. Use code to check known
  coverage requirements. Missing evidence and evidence of a defect need different
  continuations.
- Bundle independent and speculative questions over the same state. A question
  cannot see another question's answer. A question that depends on a premise
  carries that premise in its own wording; ask again after evidence changes when
  a dependent judgment needs it.
- Keep raw answers useful for inspection and different policies. Inspect actual
  state, wording, selection, and resulting action when a program fails. Early lab
  outcomes are examples to learn from, not universal restrictions on Jev.

Treat measurements as evidence about their particular fixtures; test your
own wording, evidence and decisions rather than treating outcomes as general
rules.

## A checklist example

A checklist can distinguish satisfied requirements, a demonstrated defect,
conflicting artifacts, and insufficient evidence. Name the requirements and give
each alternative an applicable condition. Every alternative here carries the same
kind of payload — the verdict the program acts on — so `J.takenUnder` settles it
under a policy with no handler list to keep in step. Keep the task and artifact
identities in the real packet. The synthetic example below illustrates reading a
selection; it does not authorize a merge or establish semantic correctness from a
diff stat.

```haskell
let diffStat = "src/Retry.hs | 24 ++++--\ntests/RetrySpec.hs | 31 +++++" :: Text
let testOutput = "PASS 14 tests, 0 failures" :: Text
let gate = J.choice "Which of these describes the candidate?"
      (J.alt #all_present "The diff touches only the named paths, the test output shows every named check passing, and a reviewer recorded the scope those checks establish" ("merge: every item of the checklist holds" :: Text)
        J..| J.alt #one_absent "At least one of the named paths, the passing check output, or the recorded review scope is missing" "repair: an item of the checklist does not hold"
        J..| J.alt #contradicts "All three are present, but the diff and the test output disagree about what was checked" "escalate: the artifacts contradict each other"
        J..| J.alt #insufficient_evidence "The state does not carry what the checklist needs to be decided: a path named in `owned_paths` appears in no line of `diff_stat`, or `test_output` names none of the checks" "ask again: name the missing field and re-ask")
answer <- J.ask1 (J.state (#owned_paths := (["src/Retry.hs", "tests/RetrySpec.hs"] :: [Text]) :& #diff_stat := diffStat :& #test_output := testOutput :& #review_scope := ("retry bounds only" :: Text))) gate
case answer of
  Left err -> "jev unavailable: " <> T.pack (show err)
  Right a -> case J.takenUnder J.careful a of
    Left doubt -> "hold: " <> doubt.why
    Right (J.Settled verdict) -> a.key <> " -> " <> verdict <> "; " <> J.explain J.careful a
```

`J.careful` is the policy for a reviewed, test-passing diff. Use `J.strict` for
a diff nobody reviewed, or for anything that leaves a receipt, and `J.lenient`
for a read-only pick. `J.explain` states in one line why the answer settled or
doubted, with the numbers behind it; put that in the notice you send, not the
raw distribution. A `Left J.Doubt` already carries that line as `doubt.why`.

`insufficient_evidence` is not a verdict on the candidate — it says the packet
was wrong. Treat it like `Left J.Doubt`: name the exact missing field, get that
field (another git command, another read, a request back to the child), and ask
again. Never merge on it, never repair on it, never fall through to the next
branch as though a decision had been made.

## The payload is the continuation

Alternatives carry the thing that runs, not a key string you later interpret.
Build the prepared continuations, let Jev select one, and run the selection.
The wording is model-facing; the payload is yours. `J.many #label key wording
rows` offers candidates computed at runtime — lines, tests, edges, table rows —
and its handler receives the row itself, so there is nothing to dereference.

```haskell
let failing = "session::retry_is_bounded" :: Text
let runners =
      [ ("rerun_one", "The output names exactly one failing test in one target", either (const "rerun unavailable") id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [failing] [bash|echo "would rerun $1"|]))
      , ("rerun_suite", "The output names failures in more than one target", either (const "suite unavailable") id . Cmd.stdout <$> Cmd.run [bash|echo "would rerun the suite"|])
      ]
answer <- J.ask1 (J.state (#test_output := ("FAIL [ 0.2s] tidepool-runtime session::retry_is_bounded" :: Text)))
  (J.choice "Which prepared continuation matches this test output?"
    (J.alt #inspect_by_hand "The output names neither a single test nor a target" (pure ("reading the failure by hand" :: Text))
      J..| J.many #rerun (\(k, _, _) -> k) (\(_, w, _) -> w) runners))
next <- either (const (pure "jev unavailable; inspecting by hand")) (\a -> J.handle a (#inspect_by_hand id J..| #rerun (\_ (_, _, action) -> action))) answer
next
```

Handlers are found by their label, not by position, so they may be written in
any order and a runtime group is handled through its label exactly as any other
alternative is. A missing handler, an extra one, or a duplicate is a compile
error naming the label.

One candidate list serves several questions in the same packet: bind it once and
draw a `J.many` and a `J.each` from it.

```haskell
let candidates = [("retry", "src/Retry.hs: retry loop and backoff" :: Text), ("fetch", "src/Fetch.hs: HTTP client and timeouts")]
let packet =
      #best := J.choice "Which file explains the timeout?"
             (J.alt #none "No file in this set is on the timeout path" ("none", "")
               J..| J.many #file fst snd candidates)
        :& #per := J.each fst (\(k, d) -> #relevant := J.noul ("Is " <> k <> " (" <> d <> ") on the path the timeout takes?")) candidates
        :& #fixed := J.noul "Given that the retry loop changed yesterday, is the timeout already fixed?"
answer <- J.ask (J.state (#failure := ("fetch times out after 3 retries" :: Text))) packet
fmap (\r -> (either (.why) (\(J.Settled (k, _)) -> k) (J.takenUnder J.lenient r.best), [(k, s.relevant.yes) | ((k, _), s) <- r.per], r.fixed.yes)) answer
```

When several items may each qualify, ask one `noul` per item with `J.each`, not
one `choice` over the items: a choice distribution is relative, a noul per item
is not. Measured on the cell above: `best` flips when the descriptions are
dropped or the question is rephrased while the per-file nouls hold, so inspect
whether you need independent membership or a competing selection. A Choice is
useful when you want one winner; independent questions answer which items
qualify without forcing them to compete. A packet's questions cannot see each
other's answers, so a question that only makes sense under a premise states that
premise in its own wording, as `#fixed` does above. A second call is appropriate
after fetching evidence the first selection identifies.

For alternatives ordered by cost or urgency, `J.score` grades against a rubric
of one to ten `J.level`s, lowest first, each carrying the result `J.grade floor`
returns when the score lands on it, so there is no list of outcomes to keep in
step; `J.graded` adds the level's label for a journal entry and `J.massAtOrAbove
#level` gives the raw number. A score is right only for a genuinely ordered,
mutually exclusive situation; reach for a noul or a choice first.

When the answer resolves nothing useful, return to ordinary reasoning with the
packet's evidence still bound in the cell. Nothing is recomputed, and a
recurring handback is the signal to write that branch by hand. The `doc jev`
topic points back to this skill as its canonical reference.

## Reusable working patterns

- **Select a prepared action.** Construct typed payloads in code, ask which
  condition applies, and dispatch the settled selection. Return unresolved cases
  and service failures explicitly.
- **Gather evidence until a question resolves.** Ask the destination question
  alongside possible next reads. Use next-read answers only while unresolved.
  Keep a budget and distinguish unresolved evidence from budget exhaustion.
- **Read with intent.** Include the assignment and relevant recent decisions.
  Preserve the distinction between instructions, observations, and quoted reports.
- **Check claims separately.** Supported, contradicted, not stated, and conflicting
  reports are different findings. A test name alone does not show its assertions.
- **Choose useful context.** Select source spans or references while preserving
  addresses to omitted material. Record excerpt scope; an unseen fact cannot be
  recovered through confidence thresholds.
- **Respond to events.** Installed actor event sources can invoke authored
  handlers that ask Jev and run a continuation. Code handles known lifecycle
  transitions, waiting, and authority.
- **Learn from failures.** Keep a small set of real packets and outcomes, including
  missing intent, missing evidence, and a mistaken action. Improve the wording,
  evidence assembly, or code that caused the failure. Compare behavior, not exact
  probability equality.

Historical experiments live in plans/jev-lab when available in the project.
Their numerical findings apply to those fixtures and questions. No fixed item
count, wording rule, or confidence floor establishes general reliability.

## Jev inside an actor

The same `J.ask` works inside a record actor's handler — the loop does not have
to come back to a model to make a semantic decision. Add the effect type to the
row (`import Tidepool.Effects.Core (Jev)`, then
`LocalEffects MyActor '[Replies, Actor, Notifications, Jev]`) and call it from
the handler exactly as in a cell. The row is checked against the launching
actor's ceiling, so a handler cannot acquire judgment its creator does not have.

Record every answer in the actor's own state as data — the key, the mass, the
confidence, `J.resolvedModel`, and the action taken — and give the record one
`Call` the owner reads the state through. A judgment nobody can inspect
afterwards is the one failure mode that costs more than the turn it saved: the
whole point of routing in Haskell is that the root can read what was decided
without re-deriving it. `exomonad-orchestrate` is that pattern written out.

Not executable on its own: it needs a live child to observe.

```haskell
let classify :: Text -> Handler [(Text, Text)] MyEffects (); classify output = do
      answer <- J.ask1 (J.state (#check_output := output))
        (J.choice "Which statement describes `check_output`?"
          (J.alt #formatting "`check_output` contains a formatting diff and no test failure" ("run the formatter" :: Text)
            J..| J.alt #lint "`check_output` names a lint by its rule name and no test fails" "fix the named lint"
            J..| J.alt #test_failure "`check_output` contains a line beginning `assertion` or `panicked at`" "read the failing assertion"
            J..| J.alt #insufficient_evidence "`check_output` is empty or does not name a tool, a rule or a test" "ask for the full check output"))
      case answer of
        Left err -> modify' (++ [("jev_unavailable", T.pack (show err))])
        Right a -> modify' (++ [(either (const "doubt") (\(J.Settled act) -> act) (J.takenUnder J.lenient a), a.key <> " " <> T.pack (show a.confidence))])
```

For a compiled command → choice → effectful continuation example, read
[recent changes](references/recent-changes.md). It handles output failure, doubt,
and the unresolved alternative separately.
