Jev is TypeSafe's judgment model: a cheap, fast "System 1" call beside your
own reasoning. Use it for the small semantic decisions that come up inside a
cell — routing, triage, relevance, whether the evidence you have already
answers the question, or which of several prepared continuations to take.
Every role may call it; calls run in tens to hundreds of milliseconds. A
judgment is evidence, not authority: code and the task's policy determine which
checks, reviews, and permissions an action requires. A choice selects a payload you already built
(a command, a line reference, a continuation); code runs that payload.

**Cost.** A call is cheap — about 200 ms — and the per-run cap is effectively
unlimited. Call it per item, per file, per candidate, inside a loop; do not
ration it the way you'd ration a model turn. Ten Jev calls that save one
model turn are a win. One Jev-dense cell — a shell listing, a read per
candidate, one packet, one round trip — that replaces a chain of five
read-then-judge model turns is a much bigger one. Plan loops against the
200 ms; prefer building one packet-rich cell over stepping through evidence
by hand.

The library is `jev-dsl`, compiled from the revision this workspace pins and in
scope qualified as `J`
(`import qualified Jev.Operators as J`). The two packet operators `:=` and
`:&` are also in scope unqualified; everything else is `J.`-qualified,
including `J..|`. A packet or offer bound in one cell and reused in a later one
keeps its inferred type.

## The call

Three host-bound entry points, no transport to thread and no raw JSON to
post:

- `J.ask1 state question` — one question, one answer, the default model.
- `J.ask state packet` — a whole packet, many questions, one round trip.
- `J.askWith model state packet` — a packet against an explicit model.

`J.state fieldPacket` is the shared context every question in the packet sees,
written exactly as a question packet is — `#task := ("…" :: Text) :& #checks
:= someList` — with each field keeping its own Haskell type. `J.rawState value`
sends a `Value` built with `object`/`.=` (from `Tidepool.Aeson`) as given, for a
shape the field packet leaves out; its fields cannot then be named. `J.ask1`
returns `Either J.JevError answer`; `J.ask`/`J.askWith` return
`Either J.JevError (J.Response ...)`. `JevError`
covers both call failures and decode problems — an unconfigured key, a cap,
a transport failure, a bad HTTP status, a timeout, and a malformed body are
all distinct cases a cell should branch on, typically by falling back to its
own judgment or handing back to the current model turn. Read the error,
don't retry blindly.

## Authoring essentials

- **Packets, not records.** The DSL is packet-shaped (`#key := question`)
  rather than a declared-record front, because a stateful session writes a
  new packet every turn and per-packet field declarations would shadow
  selectors and pollute scope across turns.
- **One packet per semantic boundary.** Put everything the current evidence
  can answer into it; don't ask a second packet just because the first
  answer arrived — ask again only when a read, a command, or a reply has
  changed the world.
- **`choice`** offers alternatives built from `J.alt #key "wording" payload`
  chained with `J..|`, plus `J.many #key rowKey rowWording rows` for
  runtime-computed candidates (lines, edges, diagnostics), whose handler
  receives the row itself. Always chain in an
  exit alternative — a no-match key, or a `defer_to_model`-style key when
  resolving needs judgment the packet can't supply. A `choice` without an
  exit still picks something; it can't say "none of these."
- **`noul`** asks a yes/no likelihood question; read it as `a.thing.yes`.
- **`score`** grades against an ordered rubric built from
  `J.level #key "wording" result` chained with `J..|`; `J.grade floor a`
  returns the result written beside the level the score landed on,
  `J.graded` returns it with that level's label, and `J.massAtOrAbove #key a`
  gives a movable threshold. The fields are `.expectation`, `.confidence`
  and `.masses`.
- **`each`** asks one question — or one nested packet — per row:
  `#per := J.each rowKey (\row -> J.noul (wordingFor row)) rows`. Each row
  comes back beside its own answer, so there is nothing to look up.
- **Packets** are built as `#key := question :& #other := question`. Nothing
  terminates the chain, and two packets join with the same `:&`, so a shared
  set of questions is an ordinary value. Nested packets flatten to dotted wire
  paths. A duplicate label is a compile error naming it.
- **Reading answers** uses record dot straight off the `Response`: `r.next`,
  `r.enough`, `r.children` (a list of `(row, subAnswers)`), with `J.answers r`
  for handing the whole packet to a function. Each answer cell
  carries its own fields: a choice has `.key`, `.mass`, `.margin`,
  `.confidence` and `.masses`; a noul has `.yes`; a score has `.expectation`,
  `.masses` and `.confidence`. A whole
  `Response` displays as an object of those fields, so ending a cell with the
  bound `answer` shows every question's distribution without a projection.
- **`J.handle answer handlers`** eliminates a choice exhaustively, handlers
  found by label rather than position — a handler list is an ordinary value, so
  bind it once and reuse it for the winner and for every contender.
- **`J.contenders floor answer handlers`** reads every alternative at or above a
  mass floor, best first, not just the winner; a near tie is itself a typed
  outcome worth branching on.
- **`J.settle policy answer handlers`** applies one of the three named policies —
  `J.lenient`, `J.careful`, `J.strict` — and returns
  `Either J.Doubt (J.Settled p r)`: the handler's result for the alternative
  that won, or a typed `J.Doubt { cause, why }` (`NearTie`, `Underweight`,
  `Unconfident`). Choose the
  policy by what the answer authorizes, not by the question's wording.
  `J.takenUnder policy answer` is the same without handlers when every
  alternative carries the same type of payload, `J.judge`/`J.holds` do it for a
  noul, and `J.explain policy answer` states in one line why it settled or
  doubted — put that in the notice, not the raw distribution.
- Keys and wording are model-facing: name alternatives by what choosing them
  means (`use_witness`, `not_in_file`, `defer_to_model`), never `a`/`b`/a
  counter. State is a typed field packet, built once and reused across
  the questions that need it; `J.field #name state` renders a checked
  reference to one of its fields inside wording.
- **A shared candidate set is an ordinary Haskell list.** Bind the rows once and
  draw both a `J.many` and a `J.each` from them when several questions in the
  same packet range over the same candidates.

## Evidence, intent, and uncertainty

Include task intent when it changes the answer. The same diagnostic can require
updating callers or restoring a definition. Keep authoritative artifacts separate
from reports, and use code for known completeness and ownership checks.

Write alternatives as comparable conditions on the supplied state. Include a
described exit when none may fit. A mass of 1.0 means no offered alternative
competes; inadequate evidence or options can still produce that result.

J.settle returns the winning alternative through its handler, whatever that
alternative means. A confident item_missing is a Right too. The handler that ran
is what decides; handle doubt and service failure explicitly. Confidence does not
establish evidence coverage or authority.
Use the named policies as starting points and evaluate the resulting behavior
for your actual task and consequences.

Bundle independent and speculative questions over the same state. They cannot
see one another's answers, so a question that only holds under a premise states
that premise in its own wording. A second call is useful when a previous answer
leads to new evidence. Keep raw judgments available for inspection and reuse.

Preserve full evidence or recoverable references. Excerpts need scope and source
addresses. A display budget must not silently delete the fact a judgment needs.
Bound command results show a compact summary while retaining the full observation.
Use Cmd.quiet for unbound command observations and small output projections to
keep large retained values out of the conversation.

See exomonad-jev for worked patterns. Historical lab results apply to their fixtures;
they are not universal rules about question wording, thresholds, or candidate count.

## A worked cell

Choosing which of several retained child results to inspect first, from a
handback exit and one `choice` over the children. Every alternative carries the
same kind of payload — what to do next — so the winner is read with
`J.takenUnder` and there is no handler list to keep in step:

```haskell
let results = [("child-1", "retry test failed in the fetch target"), ("child-2", "unrelated formatting diff")] :: [(Text, Text)]
answer <- J.ask1 (J.state (#failing_check := ("test-target actor retry" :: Text)))
  (J.choice "Which retained child result is most likely to explain the failure?"
    (J.alt #inspect_none "None of these looks relevant yet" ("", "read them in order")
      J..| J.many #child fst snd results))
either (\err -> "jev unavailable (" <> T.pack (show err) <> "), reading in order")
  (\a -> either (.why) (\(J.Settled (label, summary)) -> label <> ": " <> summary) (J.takenUnder J.lenient a)) answer
```

A `Left` here is not fatal: the cell falls back to its own policy (read in
order) and keeps going. Nothing about the fallback needs Jev; that is the
point — the tree runs without it, and Jev makes it faster.

## A packet over one candidate list

A cell that asks one packet over a shared candidate set: a choice drawn from
the rows, and one relevance question per row. Continuation lines of a multi-line
`let` must be indented past the bound name; a line that starts at the name's
column begins a new binding and fails to parse.

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

The last line keeps only plain values: the settled key or the doubt's line, a
relevance likelihood per file, and the likelihood under the premise. Ending
the cell with the bound `answer` instead displays every distribution, which
is what you want the first few times you write a packet.

## A Jev-dense cell

One shell command lists the candidates and one read per candidate gathers
evidence, bound as ordinary values. Use bounded previews when suitable, retain their paths for expansion, and use
Cmd.quiet for unbound commands when gathering evidence for code. Keep complete results available where
needed; display limits and evidence completeness are different contracts.

```haskell
listed <- Cmd.stdout <$> Cmd.run [bash|ls|]
let names = either (const []) T.lines listed
previews <- forM names $ \n ->
  (n,) . either (const "") id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [n] [bash|head -n 20 -- "$1"|])
```

Then one packet judges all of them and one round trip returns the shortlist:

```haskell
let packet =
      #enough := J.noul "Is a 20-line preview enough to judge each file?"
        :& #worth_reading := J.each fst (\(n, p) -> #keep := J.noul ("Worth reading " <> n <> " in full for this review? Its first 20 lines are:\n" <> p)) previews
answer <- J.ask (J.state (#task := ("triage files before a focused review" :: Text))) packet
fmap (\r -> (r.enough.yes, [(n, s.keep.yes) | ((n, _), s) <- r.worth_reading])) answer
```

Two cells replace the several model turns it takes to read files one by one
and judge each in turn, and only the shortlist, never the file text, reaches
the model's own context.

## Patterns

- **Locate, then edit.** Number the lines, hunks, or declarations
  deterministically and offer them as `J.many` candidates with a no-match
  exit; the retained payload is the exact row the edit runs against.
  The file text never has to enter the model's own context.
- **Independent membership, not a ranking.** When several items may each
  qualify (which children are relevant, which posters match), ask one
  `noul` per item with `J.each`, not one `choice` over the items — a
  `choice` distribution is relative, a `noul` per item is not.
  Use a `choice` alongside only when exactly one must win.
- **Ordered ladders as `score`.** For alternatives ordered by cost or
  urgency, write levels as concrete situations ("no action depends on this"
  … "continuing now invalidates work") and carry the outcome beside each
  level; `J.grade`, `expectation` and `J.massAtOrAbove` give a movable
  threshold without a second call.
- **Batch related questions into one packet.** A packet is a map of
  independent questions evaluated over one shared state, not a sequence —
  they can't see each other's answers. A question that only makes sense under
  a premise carries that premise in its own wording, instead of a follow-up
  call.
- **Always offer an exit.** Every `choice` that might have no good answer
  needs a no-match key; every packet whose resolution might need judgment
  beyond the state needs a `defer_to_model`-shaped key that hands back to
  the current model turn rather than guessing among bad options.
- **Hand back, don't fail.** When the packet's answer doesn't resolve
  anything useful, return to ordinary reasoning with the packet's evidence
  still bound in the cell — nothing is recomputed, and a recurring handback
  is a signal to add the missing branch by hand.

A call costs a small fraction of a cent at published rates. Prefer one
question-rich packet over several thin ones when the evidence for all of
them is already at hand — see Cost, above, for the loop-level version of
this rule.

skill: exomonad-jev
