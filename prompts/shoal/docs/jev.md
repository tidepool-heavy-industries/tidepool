Jev is TypeSafe's judgment model: a cheap, fast "System 1" call beside your
own reasoning. Use it for the small semantic decisions that come up inside a
cell — routing, triage, relevance, whether the evidence you have already
answers the question, or which of several prepared continuations to take.
Every role may call it; calls run in tens to hundreds of milliseconds. A
judgment is evidence, not authority: it never substitutes for review, for a
check, or for your own decision to merge, stop, or steer. It also never
generates the action itself — a choice selects a payload you already built
(a command, a line reference, a continuation), and the payload runs, never
the model's wording.

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
- **`noul`** asks a yes/no likelihood question; read it with `J.yes`.
- **`score`** grades against an ordered rubric built from
  `J.level #key "wording"` chained with `.|`; read with `J.levelOf`,
  `J.expectation`, `J.massAtOrAbove`.
- **Packets** are built as `#key := question :& ... :& J.Nil`; two packets
  join with `J.++.`. Nested packets flatten to dotted wire paths. A
  duplicate label is a compile error naming it.
- **Reading answers** uses record dot on the `Answers` value: `a.next`,
  `a.enough`, `a.children` (a list of `(key, subAnswers)`).
- **`J.handle`/`J.onMany`** eliminate a choice exhaustively, in the
  alternatives' declaration order — a handler list is an ordinary value, so
  bind it once and reuse it for the winner and for every contender.
- **`J.contenders floor answer`** reads every alternative at or above a mass
  floor, best first, not just `J.chosen`; a near tie is itself a typed
  outcome worth branching on.
- **`J.accept policy answer`** applies a
  `J.Policy {J.minMass, J.minMargin, J.minConfidence}` (the fields are
  qualified too) and returns the selection or a typed `J.Doubt`
  (`NearTie`, `Underweight`, `Unconfident`). Set thresholds from what you
  observe, not by guessing. `fmap J.selectedKey` turns an accepted selection
  into plain `Text` for display or a later cell.
- Keys and wording are model-facing: name alternatives by what choosing them
  means (`use_witness`, `not_in_file`, `defer_to_model`), never `a`/`b`/a
  counter. State is structured JSON context, built once and reused across
  the questions that need it.
- **`J.pool #name [...]`** declares a shared candidate set once when several
  questions in the same packet range over the same alternatives; draw on it
  with `J.manyFrom`/`J.eachIn`/`J.askAbout` instead of repeating wording.

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
    Right a -> pure (J.handle (J.chosen a) (J.onMany (\_ payload -> payload)))
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
let policy = J.Policy { J.minMass = 0.5, J.minMargin = 0.2, J.minConfidence = 0.5 }
answer <- J.ask (J.state (object ["failure" .= ("fetch times out after 3 retries" :: Text)])) packet
fmap (\r -> let a = J.answers r in (fmap J.selectedKey (J.accept policy a.best), [(k, J.yes s.relevant) | (k, s) <- a.per], J.yes a.fixed)) answer
```

The last line keeps only plain values: the accepted key or the doubt, a
relevance likelihood per file, and the likelihood under the premise.

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

Cost and latency: a call is cheap, typically tens to a few hundred
milliseconds and a small fraction of a cent at published rates. Prefer one
question-rich packet over several thin ones when the evidence for all of
them is already at hand.
