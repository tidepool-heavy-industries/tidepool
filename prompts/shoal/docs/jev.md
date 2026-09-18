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

The vendored library is `jev-dsl`, in scope qualified as `J`
(`import qualified Jev.Operators as J`). The two packet operators `:=` and
`:&` are also in scope unqualified; everything else is `J.`-qualified,
including `J.Nil`, `J..|` and `J.++.`. A packet or offer bound in one cell
and reused in a later one keeps its inferred type. A cell that calls Jev
needs two pragmas, cell-local:

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
```

## The call

Three host-bound entry points, no transport to thread and no raw JSON to
post:

- `J.ask1 state question` — one question, one answer, the default model.
- `J.ask state packet` — a whole packet, many questions, one round trip.
- `J.askWith model state packet` — a packet against an explicit model.

`J.state someValue` wraps the JSON context every question in the packet
sees; build the value with `object`/`.=` (from `Tidepool.Aeson`) for
anything beyond a single field. `J.ask1` returns `Either J.JevError answer`;
`J.ask`/`J.askWith` return `Either J.JevError (J.Response ...)`. `JevError`
covers both call failures and decode problems — an unconfigured key, a cap,
a transport failure, a bad HTTP status, a timeout, and a malformed body are
all distinct cases a cell should branch on, typically by falling back to its
own judgment or handing back to the current model turn. Read the error,
don't retry blindly.

## Authoring essentials

- **One packet per semantic boundary.** Put everything the current evidence
  can answer into it; don't ask a second packet just because the first
  answer arrived — ask again only when a read, a command, or a reply has
  changed the world.
- **`choice`** offers alternatives built from `J.alt #key "wording" payload`
  chained with `.|`, plus `J.many [(key, wording, payload), ...]` for
  runtime-computed candidates (lines, edges, diagnostics). Always chain in an
  exit alternative — a no-match key, or a `defer_to_model`-style key when
  resolving needs judgment the packet can't supply. A `choice` without an
  exit still picks something; it can't say "none of these."
- **`noul`** asks a yes/no likelihood question; read it as `a.thing.yes`.
- **`score`** grades against an ordered rubric built from
  `J.level #key "wording"` chained with `.|`; read `.nearest`, `.expectation`
  and `.confidence`, or `J.massAtOrAbove` for a movable threshold.
- **Packets** are built as `#key := question :& ... :& J.Nil`; two packets
  join with `J.++.`. Nested packets flatten to dotted wire paths. A
  duplicate label is a compile error naming it.
- **Reading answers** uses record dot on the `Answers` value: `a.next`,
  `a.enough`, `a.children` (a list of `(key, subAnswers)`). Each answer cell
  carries its own fields: a choice has `.key`, `.mass`, `.margin`,
  `.confidence` and `.masses`; a noul has `.yes`; a score has `.expectation`,
  `.nearest`, `.masses` and `.confidence`; a `Selected` has `.key`. A whole
  `Response` displays as an object of those fields, so ending a cell with the
  bound `answer` shows every question's distribution without a projection.
- **`J.handle`/`J.onMany`** eliminate a choice exhaustively, in the
  alternatives' declaration order — a handler list is an ordinary value, so
  bind it once and reuse it for the winner and for every contender.
- **`J.contenders floor answer`** reads every alternative at or above a mass
  floor, best first, not just `J.chosen`; a near tie is itself a typed
  outcome worth branching on.
- **`J.accept policy answer`** applies one of the three named policies —
  `J.routing`, `J.spawning`, `J.merging` — and returns the selection or a
  typed `J.Doubt` (`NearTie`, `Underweight`, `Unconfident`). Choose the
  policy by what the answer authorizes, not by the question's wording.
  `fmap J.selectedKey` turns an accepted selection into plain `Text`, and
  `J.explain policy answer` states in one line why it was accepted or
  doubted — put that in the notice, not the raw distribution.
- Keys and wording are model-facing: name alternatives by what choosing them
  means (`use_witness`, `not_in_file`, `defer_to_model`), never `a`/`b`/a
  counter. State is structured JSON context, built once and reused across
  the questions that need it.
- **`J.pool #name [...]`** declares a shared candidate set once when several
  questions in the same packet range over the same alternatives; draw on it
  with `J.manyFrom`/`J.eachIn`/`J.askAbout` instead of repeating wording.

## Evidence, intent, and uncertainty

Include task intent when it changes the answer. The same diagnostic can require
updating callers or restoring a definition. Keep authoritative artifacts separate
from reports, and use code for known completeness and ownership checks.

Write alternatives as comparable conditions on the supplied state. Include a
described exit when none may fit. A mass of 1.0 means no offered alternative
competes; inadequate evidence or options can still produce that result.

J.accept returns the accepted winning selection, whatever it means. A confident
item_missing is a Right too. Dispatch with J.handle, and handle doubt and service
failure explicitly. Confidence does not establish evidence coverage or authority.
Use the named policies as starting points and evaluate the resulting behavior
for your actual task and consequences.

Bundle independent and speculative questions over the same state. They cannot
see one another's answers. A second call is useful when a previous answer leads
to new evidence. Keep raw judgments available for inspection and reuse.

Preserve full evidence or recoverable references. Excerpts need scope and source
addresses. A display budget must not silently delete the fact a judgment needs.
Use Cmd.quiet and small output projections to keep large retained values out of
the conversation.

See shoal-jev for worked patterns. Historical lab results apply to their fixtures;
they are not universal rules about question wording, thresholds, or pool size.

## A worked cell

Choosing which of several retained child results to inspect first, from a
handback exit and one `choice` over the children:

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}

pickChild :: [(Text, Text, Response Report)] -> Eff effects (Maybe (Response Report))
pickChild results = do
  let ctx = J.state (object ["failing_check" .= "test-target actor retry"])
      offers = J.alt #inspect_none "None of these looks relevant yet" Nothing
        J..| J.many [(label, String summary, Just r) | (label, summary, r) <- results]
  answer <- J.ask1 ctx
    (J.choice "Which retained child result is most likely to explain the failure?" offers)
  case answer of
    Left err -> do
      say ("jev unavailable (" <> show err <> "), falling back to reading in order")
      pure (fmap (\(_, _, r) -> r) (listToMaybe results))
    Right a -> pure (J.handle a.chosen (J.onMany (\_ payload -> payload)))
```

A `Left` here is not fatal: the cell falls back to its own policy (read in
order) and keeps going. Nothing about the fallback needs Jev; that is the
point — the tree runs without it, and Jev makes it faster.

## A packet with a pool

A cell that asks one packet over a shared candidate set: a choice drawn from
the pool, one relevance question per entry, and a question under a premise.
Continuation lines of a multi-line `let` must be indented past the bound
name; a line that starts at the name's column begins a new binding and
fails to parse.

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let candidates = [("retry", "src/Retry.hs: retry loop and backoff" :: Text), ("fetch", "src/Fetch.hs: HTTP client and timeouts")]
let files = J.pool #files [(k, String d, k) | (k, d) <- candidates]
let packet =
      #files := files
        :& #best := J.choice "Which file explains the timeout?" (J.manyFrom files J..| J.alt #none "None of these files" "")
        :& #per := J.eachIn files (\r -> #relevant := J.askAbout r "Is this file relevant to the timeout?" :& J.Nil)
        :& #fixed := J.given "The retry loop changed yesterday" (J.noul "Is the timeout already fixed?")
        :& J.Nil
answer <- J.ask (J.state (object ["failure" .= ("fetch times out after 3 retries" :: Text)])) packet
fmap (\r -> let a = J.answers r in (fmap J.selectedKey (J.accept J.routing a.best), [(k, s.relevant.yes) | (k, s) <- a.per], a.fixed.yes)) answer
```

The last line keeps only plain values: the accepted key or the doubt, a
relevance likelihood per file, and the likelihood under the premise. Ending
the cell with the bound `answer` instead displays every distribution, which
is what you want the first few times you write a packet.

## A Jev-dense cell

One shell command lists the candidates and one read per candidate gathers
evidence, bound as ordinary values. Use bounded previews when suitable, retain their paths for expansion, and use
Cmd.quiet when gathering evidence for code. Keep complete results available where
needed; display limits and evidence completeness are different contracts.

```haskell
listed <- Cmd.stdout <$> Cmd.run [bash|ls|]
let names = either (const []) T.lines listed
previews <- forM names $ \n ->
  (n,) . either (const "") id . Cmd.stdout <$> Cmd.run (Cmd.withArguments [n] [bash|head -n 20 -- "$1"|])
```

Then one packet judges all of them and one round trip returns the shortlist:

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let files = J.pool #files [(n, String p, n) | (n, p) <- previews]
let packet =
      #files := files
        :& #enough := J.noul "Is a 20-line preview enough to judge each file?"
        :& #worth_reading := J.eachIn files (\r -> #keep := J.askAbout r "Worth reading in full for this review?" :& J.Nil)
        :& J.Nil
answer <- J.ask (J.state (object ["task" .= ("triage files before a focused review" :: Text)])) packet
fmap (\r -> let a = J.answers r in (a.enough.yes, [(k, s.keep.yes) | (k, s) <- a.worth_reading])) answer
```

Two cells replace the several model turns it takes to read files one by one
and judge each in turn, and only the shortlist, never the file text, reaches
the model's own context.

## Patterns

- **Locate, then edit.** Number the lines, hunks, or declarations
  deterministically and offer them as `many` candidates with a no-match
  exit; the retained payload is the exact reference the edit runs against.
  The file text never has to enter the model's own context.
- **Independent membership, not a ranking.** When several items may each
  qualify (which children are relevant, which posters match), ask one
  `noul` per item over a `pool`, not one `choice` over the items — a
  `choice` distribution is relative, a `noul` per item is not.
  Use a `choice` alongside only when exactly one must win.
- **Ordered ladders as `score`.** For alternatives ordered by cost or
  urgency, write levels as concrete situations ("no action depends on this"
  … "continuing now invalidates work"); `expectation` and `massAtOrAbove`
  give a movable threshold without a second call.
- **Batch related questions into one packet.** A packet is a map of
  independent questions evaluated over one shared state, not a sequence —
  they can't see each other's answers. Ask a dependent question under
  `J.given premise` for each likely premise instead of a follow-up call.
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

skill: shoal-jev
