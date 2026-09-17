# Microprogram patterns for Jev in resident cells

Recorded 2026-09-16. Authoring idioms for notebook cells that use the
single-operation Jev effect. These are patterns the model-facing library
should make easy and its documentation should teach. Names are illustrative.

## The smallest use must be effortless

Most cells want one judgment and one action:

```haskell
group <- pickOr handBack "Which diagnostic explains the failure?" (describe diagnostics)
capture (readSpan group.span)
```

One Choice over checked candidates with the handback exit supplied by the
helper, then a Bash call. No pool, no premise-prefixed extras, no competing
hypotheses. Everything richer in this document is available, never required.
If the tiny form is not one line, the library is wrong.

## The baseline

Write a helper. Let Jev carry its routine semantic branches. Fall back into
the current conversation when the tree runs out. Resume with everything still
in scope. That is the whole capability; run-ahead, request interception, and
wake suppression are later applications of it, not prerequisites.

## The cadence rule

Ask one packet per semantic boundary. A boundary is where Haskell has new
evidence or new candidates that the previous packet could not have judged.
Between boundaries, act deterministically on the previous packet's answers.
Concretely:

- Gather everything the current evidence can answer into one packet, including
  speculative questions with stated premises.
- Do not ask a second packet merely because a first answer arrived. Ask again
  when a read, a command, or a reply has changed the world.
- Three packets in one cell is normal for an investigation; twenty is a sign
  the cell is using Jev as a loop counter.

## Candidate pools

When several questions range over the same alternatives, describe the
alternatives once in state and reference their keys:

```haskell
data Expand mode = Expand
  { edges        :: mode :- Pool EdgeRecord Edge
  , nextEdge     :: mode :- Choice (From "edges" `Plus` Stops)
  , relevance    :: mode :- Each (Over "edges") Relevance
  , witnessEdge  :: mode :- Choice (From "edges" `Plus` NoWitness)
  }
```

Descriptions go to state under `edges`; the three questions carry keys with
null descriptions plus their extra alternatives. Payloads (`Edge`) stay local.
This halves tokens and keeps one source of truth.

## Semantic keys

Keys are model-facing. Name alternatives by what choosing them means:
`follow_publication_gate`, `stop_with_current_witness`,
`unresolved_no_useful_edge`. Never `a`, `b`, `option_1`. For dynamic
candidates, derive the key from a stable, meaningful identity (a symbol name,
a commit subject slug, a decision key), not from a counter. Coalesce
alternatives with equal descriptions before the call.

## Always offer the exits

Every Choice that might have no good answer gets a no-match alternative with
a description that says what choosing it means. Every candidate set whose
coverage matters gets a separate presence Noul. A Choice always ranks the
supplied alternatives; without an exit it will pick the least bad one.

Beyond no-match, give the tree a way to say "not mine to decide". A
`defer_to_model` alternative described as "resolving this needs a design
preference or evidence outside the supplied state" lets the judgment itself
route to System 2. The current model turn is already there; the cell only has
to return. This exit is separate from confidence-based exits and needed even
when the distribution is concentrated: a confident answer among the supplied
alternatives does not establish that the helper covers the situation.

## Hand back, do not fail

A cell's outcome type has a constructor for every way the authored tree can
run out, and each carries the resident evidence:

```haskell
data Outcome
  = Located Witness Evidence
  | NeedsJudgment Inquiry Evidence Alternatives   -- the model decides, then resumes
  | Consult Owner Question Evidence               -- an explicitly authored escalation
```

Reasons to return `NeedsJudgment` are not only low confidence: two routes are
both useful and choosing is a preference; no candidate covers what the
evidence suggests; two differently framed judgments disagree; the next action
is beyond what this cell was written to decide. Journal the reason. The model
resumes over the bindings the cell left behind; nothing is recomputed.

When the same reason recurs, extend the helper. The resident declaration
environment persists, so the branch the model took by hand this time can be a
`Choice` alternative next time. The tree grows in the session.

## Premise-prefixed speculation

When the next step depends on an answer in the same packet, ask the dependent
question for each likely premise:

```haskell
  , checkIfRetry     :: mode :- Given "mechanism is actor_redelivery" (Choice Checks)
  , checkIfAdmission :: mode :- Given "mechanism is inbox_double_admit" (Choice Checks)
```

`Given` renders the premise into the instruction. Haskell consumes the one
whose premise won. This removes a round trip at the cost of a few hundred
input tokens.

## Independent judgments over one membership

When several recipients, pieces, or workers may each qualify, ask one Noul per
item, not one Choice over items. A Choice distribution is relative; a Noul per
item is a membership judgment. Use a Choice alongside only when exactly one
must be selected.

## Ordered ladders as Scores

When the alternatives are ordered by cost or severity, use a Score with levels
that are concrete situations: "background information", "useful at the next
checkpoint", "blocks the next action", "continuing now invalidates work".
Expectation plus confidence gives a threshold policy can move without a new
call.

## Locate, then edit

The frontier model's most expensive habit is reading a whole file into its
context to find the three lines it wants to change. Make the lines
candidates instead. Haskell numbers the file, or the hunks, or the
declarations from an outline, and asks one Choose over them with a
no-match exit; the retained payload is the exact line reference, and the
edit runs against it. The file never enters the model's context.

```haskell
lines  <- numbered <$> readFile path                        -- deterministic
target <- pickOr handBack policy =<< jev1 model world
            (choose "Which line begins the retry-timeout branch?" lines [noMatch "Not in this file"])
edit (spanFrom target 6) replacement                         -- retained reference
```

Two Chooses locate a start and an end; an `Each` over hunks judges which
ones a review finding applies to; a Choose over an outline picks the
declaration to extend. The semantic-find cookbook already does this over
line ids. This is the "run a command, then a tree of natural-language
conditions" shape: a notebook cell with one Bash call, one packet, and typed
branches on the answers, where an ad hoc read-then-edit turn used to cost a
model round per look.

Three cautions from Astra's review. The retained target must carry the
source revision and span, and the edit must check freshness against the
file before applying, exactly as the typed file tools plan requires. "The
file never enters context" means the frontier model's context; Jev still
receives the candidate lines, so the packet is not free. And the savings
are a measurement, not an assumption: count the frontier tokens the read
would have cost against the packet's input tokens on real edits before
claiming a fraction.

## Keep competing explanations alive (available, not mandatory)

For an investigation whose mechanism is genuinely open, when a Choice over
mechanisms splits, do not commit and do not return. Retain
every hypothesis above a floor, take the discriminating observation each
premise-prefixed question already selected for it, gather those observations,
and ask again over the enriched state with the same hypotheses plus
"neither":

```haskell
  live      <- pure (aboveFloor 0.25 judged.answers.mechanism)
  probes    <- traverse (probeFor judged) live           -- one observation each
  observed  <- traverse capture probes
  refined   <- infer (assess inquiry (enrich state observed) live)
```

The read budget bounds how many stay alive. The returned bundle either names
one supported mechanism with the evidence that separated it, or names the
survivors with the evidence that failed to separate them. Both are better
inputs to a frontier model than an early commitment.

## Reading a result

- Take the distribution, not the argmax. `pick` returns the winner, its
  mass, the runner-up, its mass, and confidence.
- A near-tie is a typed outcome. Policy decides whether to read both, ask a
  narrower packet, or return the tie to the caller.
- A Noul near 0.5 is "unknown", not "somewhat".
- Confidence below a stakes-dependent floor routes to the fallback for that
  action: read-only continuations can proceed at moderate confidence;
  anything that spends a budget or sends a message needs more.
- Structural validity and semantic acceptance are separate. A response that
  passed structure can still be a judgment the policy declines.

## The cell shape

```haskell
cell = runCell $ within limits $ do
  observed   <- capture command                       -- deterministic
  candidates <- require (options (describe observed)) -- checked construction
  judged     <- infer (packet inquiry observed candidates)
  chosen     <- require (pick policy judged.answers.target)
  evidence   <- capture chosen.command                 -- selected continuation
  final      <- infer (assess inquiry observed evidence) -- next boundary
  require (verdict policy final)
```

Deterministic observation, checked candidates, one packet, typed policy,
selected continuation, optional second boundary, typed return. The executor
sees the return value, and the trace reference retains everything else.

## Stop conditions are policy

A completion record of independent Nouls plus a pure guard is the pattern for
"am I done". Jev never returns a done bit. The guard reads margins, orders the
outcomes by consequence, and returns a typed disposition: stop with evidence,
continue with a named plan, consult an owner, escalate.

## Anti-patterns

- **Jev as a shell generator.** Never let a Choice key or description become
  a path, a command, or an actor identity. Selection returns the retained
  typed payload.
- **Jev as a graph interpreter.** Do not ask it to follow pointers or count
  hops. Compute the frontier, then ask about the frontier.
- **One broad question.** "Is this spam" hides six judgments. Ask the six.
- **Reading a duplicate-option split as calibration.** It is key bias.
- **Asking again without new evidence.** The answer will be the same.
- **Thresholds without data.** Start conservative, journal everything, and
  set thresholds from the replay corpus.
- **Extreme fan-out.** Hundreds of questions is a capacity probe, not a
  program.
