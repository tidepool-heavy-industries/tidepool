# Companion State v2 — typed structure bottoming out in Value

STATUS: DESIGN, awaiting final review before implementation. Designed
collaboratively (operator + root, 2026-08-13) from live dogfood evidence;
imposed on the companion rather than co-designed with it — the agent operates
this medium well but does not yet design it.

## Evidence this answers (from the soak)

The companion reported, accurately: all-`Text` fields cannot structurally
distinguish an operator contribution from an observation from an instruction;
nothing can be revised or retired, so attention degrades as everything
accumulates; a thread cannot wait for external input without re-asking;
mandatory per-window mutation turns deliberate quiet into meta-commentary; and
it cannot compute over its own state (the answerer sees only `render`'s prose
projection — filtering, search, and counting over memory are impossible).

## Design principle

**Typed spine, `Value` bottoms.** The spine is typed exactly as deep as the
machinery (render, combinators, GUI, checkpoint) needs to see; every level
where evolution is EXPECTED ends in an `aeson` `Value` leaf the agent edits
ad hoc with lens, no schema, no review. A `Value` pattern that proves out
GRADUATES into the spine through the proposals loop — ad-hoc, then observed
in use, then typed. Evolution speed where change is cheap; structural
guarantees where the machinery depends on them.

## Schema (HarnessTypes)

```haskell
data Provenance = FromOperator | FromAgent
data Standing   = Active | Archived | Retired

data Entry
  = Fact Text                 -- something taken as true
  | Event Text                -- something that happened
  | Quote Provenance Text     -- who said what (conversation-as-data)
  | Structured Value          -- ad-hoc structured memory (a Value bottom)
  | Note Text                 -- catch-all; nothing is unsayable

data Memory = Memory
  { mid      :: Int           -- stable id, combinator-assigned
  , born     :: Int           -- loopN at creation
  , source   :: Provenance
  , standing :: Standing
  , entry    :: Entry
  }

data ThreadStatus = Live | WaitingOnOperator | Resting | Resolved
data Thread = Thread
  { tid      :: Int
  , question :: Text
  , status   :: ThreadStatus
  , stance   :: Maybe Text    -- provisional position, revisable
  }

data State = State
  { identity  :: Text         -- one prose paragraph; wholesale, deliberate rewrites
  , loopN     :: Int          -- the clock; authored-loop bookkeeping, never edited
  , memories  :: [Memory]
  , threads   :: [Thread]
  , proposals :: [Text]       -- harness-change asks, aimed at the operator
  , scratch   :: Value        -- the sandbox (the big Value bottom)
  }
  deriving (Generic, ToJSON, FromJSON, Show)
```

Retire never deletes (audit trail); retention (below) bounds serialized size.
`Preference` deliberately absent from `Entry` v1 — it starts life in
`scratch`/`Structured` and graduates if used (eating our own graduation
pipeline first).

## The edit vocabulary (authored combinators, same module)

The answer type stays `State -> State` — the endomorphism monoid; identity is
the blessed no-change answer. Edits compose named operations; raw record
surgery remains legal but the combinators do the bookkeeping (id assignment,
`born` stamping):

```haskell
remember     :: Provenance -> Entry -> State -> State   -- mints mid, stamps born
revise       :: Int -> Entry -> State -> State          -- new content, same id/provenance
setStanding  :: Standing -> Int -> State -> State       -- retire/archive/reactivate by mid
openThread   :: Text -> State -> State                  -- mints tid, status Live
updateThread :: Int -> (Thread -> Thread) -> State -> State
propose      :: Text -> State -> State
onScratch    :: (Value -> Value) -> State -> State      -- the lens passthrough
```

The companion's living library builds macros atop these (e.g. a
`consolidate :: State -> State` archiving stale actives) — combinators are the
substrate its habits compile against.

## ReadState — the agent computes over its own state

A new answerer-row effect (row-visible, so the capability documents itself —
the lesson the three prompt-drift bugs taught):

```haskell
getState :: M State
```

Driver service: classify the `ReadStateWith` hole, resume IMMEDIATELY with
the current state (the same JSON the checkpoint holds), no operator, no model
round — `note`'s service shape exactly. Freshness is trivially correct: state
changes only at loop boundaries. `scratch` arrives as `Value`, ready for lens
queries. Archived-memory search becomes ordinary Haskell in an explore round.
Answerer row becomes `[AskUser, Fork, ReadState, Finalize T]`. Not in the
outer row (the loop already holds the state).

## Render is an attention policy

`render` selects, never dumps: `identity`; Active memories (newest first,
bounded); Live + WaitingOnOperator threads (waiting ones flagged); pending
proposals; a one-line inventory of scratch's top-level keys. Archived/Retired
content is reachable only through `getState` — that is the working-context /
archive split, expressed as a pure function.

## Authored-loop bookkeeping (mechanical, reviewable, not the model's job)

```haskell
loop st = do
  let st1 = maybe st (\m -> remember FromOperator (Quote FromOperator m) st) __operatorMsg
  edit <- runLLMTurn @(State -> State) prompt
  pure (tick (retention (edit st1)))
```

- `__operatorMsg :: Maybe Text` — a driver splice beside `__selfHarnessState`
  (small driver change): the operator's between-loops message enters memory as
  a provenance-tagged `Quote`, written by authored code — neither hidden
  harness machinery nor model transcription.
- `tick` bumps `loopN`.
- `retention` hard-drops `Retired` entries older than K loops (the state JSON
  re-splices into every loop compile; growth must be bounded). K authored,
  visible, adjustable.

## Prompt changes

- Bless identity: "a window that changed nothing finalizes `id` — a complete,
  honorable answer" (+ `note` for the optional receipt).
- Teach the combinators and `Entry` by example; the `ReadState` card arrives
  free via the generated effects section.
- Describe scratch and the graduation pipeline: "structure experiments in
  `scratch` freely; propose promotion to the typed spine when a shape earns
  its keep."

## Migration: none (punted deliberately)

The old checkpoint is archived read-only (never deleted — protected data);
the companion starts v2 fresh, `identity` optionally hand-seeded from v1's.
Losing the soliloquy-era state is acceptable; the typed spine is not worth
contorting to carry it.

## Verification before/while implementing

1. **Lens WRITES on the JIT**: `over (key "k" . _Number)`-style edits compile
   and run (reads are proven surface; writes unexercised) — a
   `jit_surface`-family probe before anything depends on `onScratch`.
2. `ReadState` round trip: effect decl + classify arm + immediate-resume
   service + a spine test (explore round reads state, finalize branches on
   it).
3. Combinator unit behavior via `companion_typechecks` + a small
   authored-module test fixture.
4. The decl-plane end-to-end acceptance keeps passing with the new row.

## Rollout

1. Land schema + combinators + render + loop in `harness-dogfooding/companion/`.
2. Land `ReadState` (decl, classify, service, row) + `__operatorMsg` splice.
3. Prompt updates; `companion_typechecks` green; targeted battery legs.
4. Archive old checkpoint; bounce; tell the companion what changed and why —
   including that its own reports drove the design.
