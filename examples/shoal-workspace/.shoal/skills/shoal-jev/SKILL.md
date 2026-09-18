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

Everything named in this skill is **shipped**: `J.ask`, `J.ask1`, `J.choice`,
`J.noul`, `J.score`, `J.pool`, `J.eachIn`, `J.askAbout`, `J.given`, `J.alt`,
`J..|`, `J.many`, `J.manyFrom`, `J.accept`, `J.explain`, `J.handle`,
`J.contenders`, `J.selectedKey`, `J.state`, `J.answers`, `J.resolvedModel`,
`J.routing`, `J.spawning`, `J.merging`. The one exception is the **effect
type** `Jev`, which a record actor's row must name and which is not re-exported
by the workbench surface: `import Tidepool.Effects.Core (Jev)`. Nothing from
`examples/shoal-workspace/.shoal/Project` appears here.

`import qualified Jev.Operators as J` is in scope; `:=` and `:&` read
unqualified.

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
let files = J.pool #files [(n, String p, n) | (n, p) <- previews]
let packet =
      #files := files
        :& #enough := J.noul "Is a 20-line preview enough to judge each file, or does judging need the whole file?"
        :& #worth_reading := J.eachIn files (\r -> #keep := J.askAbout r "Worth reading in full for this review?" :& J.Nil)
        :& J.Nil
answer <- J.ask (J.state (object ["task" .= ("triage files before a focused review" :: Text)])) packet
fmap (\r -> let a = J.answers r in (a.enough.yes, [(k, s.keep.yes) | (k, s) <- a.worth_reading])) answer
```

Use previews when they contain the evidence the question needs; label their scope
and retain paths for expansion. Keep complete values or recoverable references.
Use Cmd.quiet and small display projections to avoid flooding the conversation;
do not discard deciding evidence merely to shorten its display.

## Reading the answer

Answers are data. A choice cell has `.key`, `.mass`, `.margin`, `.confidence`
and `.masses`; a noul has `.yes`; a score has `.nearest`, `.expectation`,
`.masses` and `.confidence`; a `Selected` has `.key`. A whole `Response`
displays as an object of those fields, so ending a cell with the bound `answer`
shows every distribution — do that the first few times you write a packet, then
project what the next decision needs. `J.contenders floor a` reads every
alternative above a mass floor, best first; a near tie is a typed outcome worth
branching on. `J.handle a.chosen handlers` eliminates a choice exhaustively.

`J.accept policy a` returning `Right` means the **winning key cleared the
floors**, whatever that key is: a confident `item_missing` is a `Right`. Never
treat `Right _` as approval; dispatch on the key with `J.handle`, and let only
the one alternative that means "proceed" proceed. A selected answer still depends on the supplied
evidence. Choose review and escalation from the task's policy and consequences.
A known-answer question can help diagnose a packet; passing it does not establish
that every other answer is correct.

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
  cannot see another question's answer. Ask again after evidence changes when a
  dependent judgment needs it.
- Keep raw answers useful for inspection and different policies. Inspect actual
  state, wording, selection, and resulting action when a program fails. Early lab
  outcomes are examples to learn from, not universal restrictions on Jev.

## A checklist example

A checklist can distinguish satisfied requirements, a demonstrated defect,
conflicting artifacts, and insufficient evidence. Name the requirements and give
each alternative an applicable condition. Keep the task and artifact identities
in the real packet. The synthetic example below illustrates reading a selection;
it does not authorize a merge or establish semantic correctness from a diff stat.

```haskell
let diffStat = "src/Retry.hs | 24 ++++--\ntests/RetrySpec.hs | 31 +++++" :: Text
let testOutput = "PASS 14 tests, 0 failures" :: Text
let gate = J.choice "Which of these describes the candidate?"
      (J.alt #all_present "The diff touches only the named paths, the test output shows every named check passing, and a reviewer recorded the scope those checks establish" ()
        J..| J.alt #one_absent "At least one of the named paths, the passing check output, or the recorded review scope is missing" ()
        J..| J.alt #contradicts "All three are present, but the diff and the test output disagree about what was checked" ()
        J..| J.alt #insufficient_evidence "The state does not carry what the checklist needs to be decided: a path named in `owned_paths` appears in no line of `diff_stat`, or `test_output` names none of the checks" ())
answer <- J.ask1 (J.state (object ["owned_paths" .= (["src/Retry.hs", "tests/RetrySpec.hs"] :: [Text]), "diff_stat" .= diffStat, "test_output" .= testOutput, "review_scope" .= ("retry bounds only" :: Text)])) gate
case answer of
  Left err -> "jev unavailable: " <> T.pack (show err)
  Right a -> either (\doubt -> "hold: " <> T.pack (show doubt) <> "; " <> J.explain J.spawning a) J.selectedKey (J.accept J.spawning a)
```

`J.spawning` is the policy for a reviewed, test-passing diff. Use `J.merging`
for a diff nobody reviewed, or for anything that leaves a receipt. `J.explain`
states in one line why the answer was accepted or doubted; put that in the
notice you send, not the raw distribution.

`insufficient_evidence` is not a verdict on the candidate — it says the packet
was wrong. Treat it like `Left Doubt`: name the exact missing field, get that
field (another git command, another read, a request back to the child), and ask
again. Never merge on it, never repair on it, never fall through to the next
branch as though a decision had been made.

## The payload is the continuation

Alternatives carry the thing that runs, not a key string you later interpret.
Build the prepared continuations, let Jev select one, and run the selection.
The wording is model-facing; the payload is yours.

```haskell
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
dropped or the question is rephrased while the per-file nouls hold, so inspect whether you need independent membership or a competing selection.
A Choice is useful when you want one winner; independent questions answer which
items qualify without forcing them to compete. A packet's
questions cannot see each other's answers; ask branch-specific questions under
`J.given premise` when the relevant evidence is already available. A second call
is appropriate after fetching evidence the first selection identifies.

When the answer resolves nothing useful, return to ordinary reasoning with the
packet's evidence still bound in the cell. Nothing is recomputed, and a
recurring handback is the signal to write that branch by hand. `doc jev` is the
same material in fallback form.

## Reusable working patterns

- **Select a prepared action.** Construct typed payloads in code, ask which
  condition applies, and dispatch the accepted selection. Return unresolved cases
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
without re-deriving it. `shoal-orchestrate` is that pattern written out.

Not executable on its own: it needs a live child to observe.

```haskell
let classify :: Text -> Handler [(Text, Text)] MyEffects (); classify output = do
      answer <- J.ask1 (J.state (object ["check_output" .= output]))
        (J.choice "Which statement describes `check_output`?"
          (J.alt #formatting "`check_output` contains a formatting diff and no test failure" ()
            J..| J.alt #lint "`check_output` names a lint by its rule name and no test fails" ()
            J..| J.alt #test_failure "`check_output` contains a line beginning `assertion` or `panicked at`" ()
            J..| J.alt #insufficient_evidence "`check_output` is empty or does not name a tool, a rule or a test" ()))
      case answer of
        Left err -> modify' (++ [("jev_unavailable", T.pack (show err))])
        Right a -> modify' (++ [(either (const "doubt") J.selectedKey (J.accept J.routing a), T.pack (show a.confidence))])
```
