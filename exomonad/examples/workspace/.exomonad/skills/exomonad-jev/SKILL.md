---
name: exomonad-jev
description: "Compose semantic judgment with Haskell: packets, predicates, competing choices, rubrics and typed continuations. Use Jev as glue inside functions, agent RPC and state-machine actors when interpretation determines what runs next."
---

Jev makes semantic judgment composable inside an ordinary Haskell program.
An alternative can carry a domain value, a partially applied function or an
effectful action. Ask which condition fits, then pass its payload directly into
the continuation. A small orchestration language can use Jev in its interpreter:
classify an incoming report, choose the next question, or select a handler that
updates actor state and requests more work. Keep the exact computations in code
and put interpretation where the composition needs it.

Batch independent questions over shared evidence; sequence calls when an answer
determines what to observe next. Compose these stages as functions, reuse packets
as values, and retain results for later projections. Measure latency and usage
for the workload you are building.

Everything named in this skill is **shipped**: `J.ask`, `J.ask1`, `J.askWith`,
`J.choice`, `J.noul`, `J.score`, `J.each`, `J.optional`, `J.alt`, `J..|`,
`J.many`, `J.level`, `J.state`, `J.rawState`, `J.field`, `J.settle`,
`J.takenUnder`, `J.judge`, `J.holds`, `J.grade`, `J.graded`, `J.handle`,
`J.taken`, `J.contenders`, `J.explain`, `J.massAtOrAbove`, `J.answers`,
`J.resolvedModel`, `J.lenient`, `J.careful`, `J.strict`. The one exception is
the **effect type** `Jev`, which a record actor's row must name and which is not
re-exported by the workbench surface: `import Tidepool.Effects.Core (Jev)`.
Nothing from `.exomonad/workspace/Project` appears here.

`import qualified Jev.Operators as J` is in scope; `:=` and `:&` read
unqualified.

`J.ask1 state question` asks one; `J.ask state packet` asks a whole packet in
one round trip. Both return a typed Jev call error or a response. Use
`J.answers response` to project the answer payload; retain the response when
you need its model, usage or diagnostics. Read the typed error and fall back
to your own policy; never retry blindly.

## Compose the questions

Choose the question's structure before its wording. A `choice` models competing
explanations or actions; independent `noul` questions model predicates that can
all hold; `each` lifts a question over runtime candidates; a `score` models an
ordered rubric. This is a modeling decision: a relative winner does not prove
absolute suitability, and an ordered scale cannot represent unrelated causes.
Include an unresolved or none-applicable alternative when the candidate set can
miss the case. Keep same-evidence questions in one packet; sequence a later call
when a result determines which evidence to fetch.

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
let packet =
      #enough := J.noul "Is a 20-line preview enough to judge each file, or does judging need the whole file?"
        :& #worth_reading := J.each fst (\(n, p) -> #keep := J.noul ("Worth reading " <> n <> " in full for this review? Its first 20 lines are:\n" <> p)) previews
answer <- J.ask (J.state (#task := ("triage files before a focused review" :: Text))) packet
display (fmap (\r -> (r.enough.yes, [(n, s.keep.yes) | ((n, _), s) <- r.worth_reading])) answer)
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
Bound command results retain their complete observation without rendering the
payload automatically. Use `Cmd.quiet` for unbound command observations and
`display` on small projections when you need to inspect them; do not discard
deciding evidence merely to shorten its display.

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
with the bound `answer` retains the typed value without displaying it. Use
`display answer` while learning a packet, then display only the projection the
next decision needs.

Use `J.handle` or `J.settle` for dispatch: each handler receives the alternative's
original typed payload, including its captured values or prepared action. `.key`
names the choice in a display or journal. `.margin` is the
winner's mass less the runner-up's, and equals the mass when nothing competes,
so a margin near 1.0 usually means the alternatives were not really rivals.
`J.contenders floor answer handlers` reads every alternative at or above a mass
floor, best first, each already through the same handlers; a near tie is a typed
outcome worth branching on. `J.handle answer handlers` eliminates a choice
exhaustively with no policy at all, for when the program follows the winner
whatever it is.

A policy sets three floors: mass, margin and confidence. `J.lenient`,
`J.careful` and `J.strict` provide increasing thresholds; choose them against
actual decisions and outcomes. `J.handle` follows the winner directly;
`J.settle` adds a doubt branch when the program benefits from another read,
another question or a handback. `J.settle policy answer
handlers` returns `Either J.Doubt (J.Settled p r)`: `Right settled` carries
the handler result, available through `J.settledValue settled`, or `Left` a
`J.Doubt { cause, why }` whose `cause` is `NearTie`, `Underweight` or
`Unconfident` and whose `why` is the line a log reads. `J.takenUnder` is the
same with no handler list when every alternative carries the same type of
payload, `J.judge` and `J.holds` do it for a noul — a doubt is not a no, and
both read as `False` under `J.holds`, which is why it is a separate verb.
Choose the projection that preserves the distinctions your continuation uses.

`Right` means the winning alternative cleared the floors: a confident
`insufficient_evidence` is also a `Right` and can select a read-more continuation.
The payload and handler determine the behavior. Inspect a surprising decision's
evidence, alternatives and outcome; increasing a confidence threshold cannot
add a missing fact or an omitted alternative.

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
display (case answer of
  Left err -> "jev unavailable: " <> T.pack (show err)
    Right response -> let a = J.answers response in case J.takenUnder J.careful a of
    Left doubt -> "hold: " <> doubt.why
    Right settled -> a.key <> " -> " <> J.settledValue settled <> "; " <> J.explain J.careful a)
```

The example uses `J.careful` to demonstrate settlement. Choose thresholds from
the decisions and outcomes of your actual task; a stricter threshold cannot
supply a missing review. `J.explain` summarizes why the answer settled or doubted,
with the numbers behind it. A `Left J.Doubt` carries that line as `doubt.why`.

`insufficient_evidence` means this packet cannot establish the described
conclusion. Its continuation can name and fetch a missing field, ask the child
for more context, or return an unresolved result. A domain policy may instead
choose a remediation action; encode that choice in the alternative's payload
and handler. Keep the distinction between that action and evidence that the
candidate passed or failed.

## The payload is the continuation

A prepared `Eff effects result` is a Haskell value: constructing `Cmd.run command`
does not run the command. Let each alternative carry its own typed continuation,
then sequence the selected action through its handler, with a settlement policy
when useful. Running all
candidates before asking loses the benefit of selection. Keep pure extraction,
exact checks and known transitions in functions; use Jev where semantic evidence
selects the next effect. The same pattern works for choosing a source read, a
specialist assignment, a repair approach or an event handler's next transition.

The wording and rendered evidence go to Jev; the payload remains your original
Haskell value, including captured data and actions. Put relevant text and
structured evidence fields in `J.state` and keep the richer domain value in the
payload. `J.many #label key wording
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
next <- either (const (pure "jev unavailable; inspecting by hand")) (\response -> J.handle (J.answers response) (#inspect_by_hand id J..| #rerun (\_ (_, _, action) -> action))) answer
display next
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
display (fmap (\r -> let a = J.answers r in (either (.why) (J.settledValue) (J.takenUnder J.lenient a.best), [(k, s.relevant.yes) | ((k, _), s) <- a.per], a.fixed.yes)) answer)
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

Calibrate against decisions and outcomes, including near ties, inadequate
evidence, irrelevant candidates and confidently wrong actions. Compare a revised
packet or policy on the same retained cases; add a known-answer or deliberately
missing-evidence case to check whether the procedure can distinguish them.
Inspect both settled and doubtful outputs. Repeated agreement on the same packet
can share the same missing assumption; obtain an independent check or new evidence
when that could change a consequential action. Choose the next read by its chance
of separating live alternatives relative to its cost, and retain an unresolved
result when the available evidence or budget cannot settle it.

## Jev inside an actor

The same `J.ask` works inside a record actor's handler. The actor interprets your
control language, holds the current state and composes judgments with its next
effects. An event can update accumulated findings, ask which hypothesis to pursue,
and launch a typed agent request whose reply becomes another event. This makes
adaptive orchestration an authored program. Add the effect type to the
row (`import Tidepool.Effects.Core (Jev)`, then
`LocalEffects MyActor '[Replies, Actor, Notifications, Jev]`) and call it from
the handler exactly as in a cell. The row is checked against the launching
actor's ceiling, so a handler cannot acquire judgment its creator does not have.

Give the actor a query endpoint for the state its callers need. Keep useful
decision history there when it helps explain or revise the machine: selected
payload, evidence, action, and distributions relevant to a surprising result.
`exomonad-review`'s owner-map and repair-policy example is one composition to
adapt; a research machine can use the same structure for hypotheses and next reads.

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
        Right response -> let a = J.answers response in modify' (++ [(either (const "doubt") J.settledValue (J.takenUnder J.lenient a), a.key <> " " <> T.pack (show a.confidence))])
```

For a compiled command → choice → effectful continuation example, read
[recent changes](references/recent-changes.md). It handles output failure, doubt,
and the unresolved alternative separately.
