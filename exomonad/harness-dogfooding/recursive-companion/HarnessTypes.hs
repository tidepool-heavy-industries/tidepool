{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Durable vocabulary for the recursive-companion dogfood.
--
-- Every type here is nameable in a SESSION's row (the typed-request agent
-- row, which has no @RunLLMTurn@): "Harness" defines @loop@, whose
-- @runLLMTurn@ verb is absent from that row, and GHC compiles an imported
-- module whole, so a session-facing type declared beside @loop@ could not be
-- named by the session asked to answer with it.  'SeedQuestion' and
-- 'OperatorSteering' therefore live HERE.
--
-- The tree machinery that used to live beside them — layer proposals, the
-- operator gate, node paths, fold accounting — is GONE, not moved
-- (fork-subsumes-split step 4): the tree now emerges from a session's own
-- @async (fork \@T brief)@ calls, tracked, budgeted, and rendered by the
-- DRIVER, so the companion no longer owns a tree vocabulary at all.
module HarnessTypes
  ( -- * Checkpointed state
    State (..)
  , initialState

    -- * The seed gate ('Harness.loop'\'s opening ask)
  , SeedQuestion (..)

    -- * A session's ask for operator intent
  , OperatorSteering (..)

    -- * Rendering
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

-- ---------------------------------------------------------------------------
-- Checkpointed state
-- ---------------------------------------------------------------------------

-- | Only durable facts cross a turn boundary: the question, the turn count,
-- and the last turn's answer.  Live contexts, parked continuations and
-- in-flight fork trees are not checkpointed — a
-- process loss mid-turn reruns the turn from here.
--
-- No caps live here (fork-subsumes-split step 4): depth and descendant
-- budgets are the DRIVER's spawn-time enforcement, not companion
-- configuration.
--
-- 'lastAnswer' is the root session's own typed answer, verbatim — never
-- re-shaped (operator decision, 2026-08-23).  The tree of forked
-- sub-sessions that produced it is the driver's territory (operator page,
-- journal, budgets); nothing here re-renders it.
data State = State
  { question   :: Text
  , turnCount  :: Int
  , lastAnswer :: Maybe Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The question is deliberately EMPTY: seeding it is the OPERATOR's first
-- act, not the author's — 'Harness.loop' opens by asking for it
-- (@askUser \@SeedQuestion@) whenever it is empty, before any model session
-- runs (operator decision, 2026-08-19; a hardcoded question meant the first
-- attended run spent real model turns on a question nobody chose).
initialState :: State
initialState = State {question = "", turnCount = 0, lastAnswer = Nothing}

-- ---------------------------------------------------------------------------
-- The two operator forms
-- ---------------------------------------------------------------------------

-- | The operator's opening move ('Harness.loop'\'s seed gate): the question
-- the whole run investigates.  One field, so the derived form is a single
-- text input, presented BEFORE any model session runs.
data SeedQuestion = SeedQuestion
  { seedQuestion :: Text
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- | A session's channel for OPERATOR intent: sessions investigate and fork
-- on their own, and raise THIS ask (question posted via @note@, since a
-- shape-derived form carries no prompt text of its own) only when the
-- operator's answer would genuinely change what the session does.
data OperatorSteering = OperatorSteering
  { steeringReply :: Text
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

-- ---------------------------------------------------------------------------
-- Render — the last answer is primary
-- ---------------------------------------------------------------------------

-- | @render :: State -> Text@ — the LOCKED signature.  Domain policy only:
-- the driver composes this output with the loop-iteration count, the prior
-- compaction summary, and capability\/finalization instructions.
--
-- Emitted in order: the last turn's answer verbatim (or, pre-first-turn, the
-- opening orientation), the question, then 'companionProtocol' — see its own
-- doc for what it teaches and what it deliberately leaves to the driver's
-- framing.
render :: State -> Text
render st = case st.lastAnswer of
  Nothing ->
    [fmt|You are a recursive companion. Nothing has been answered yet.

Question: {questionLine}

Each turn asks you — one session — to investigate this question and finalize
a typed answer. Decompose by forking typed sub-answerers; the driver tracks,
budgets, and shows the operator the tree your forks grow, so your job is the
thinking and the answer, never the bookkeeping.

{companionProtocol}|]
    where
      -- The seed gate recurses into the request in the SAME cycle (see
      -- 'Harness.loop'), so this framing is sometimes rendered from a
      -- State whose question is still blank — the real question is on its
      -- way in the request card below. An empty "Question: " line would
      -- read as "no question exists" and invite re-confirming what the
      -- operator just typed; say so plainly instead.
      questionLine =
        if T.strip st.question == ""
          then "(being seeded this turn — the request below carries it; treat the request's question as authoritative)" :: Text
          else st.question
  Just answer ->
    [fmt|The previous turn's finalized answer, verbatim:

{answer}

Question: {st.question}
Turns completed: {show st.turnCount}

{companionProtocol}|]

-- | The companion-specific teaching every session in a turn's tree inherits
-- through its framing.  ONLY what the driver's own answerer framing does not
-- already cover (that framing teaches the multi-round rhythm, @fork@\/
-- @async@ batching, the fork budget, declaration persistence, and the
-- @finalize@ contract): the ancestry rules for shared code, the delegate
-- contract for repository evidence, and the operator-steering ask.
--
-- Delimited with plain @--- ... ---@ markers rather than a triple-backtick
-- fence: a markdown fence here would compete with the engine's own
-- auto-rendered request-card fence for FIRST position in the assembled
-- request.
companionProtocol :: Text
companionProtocol =
  [fmt|--- PROTOCOL: every session in this run ---

Design the RESULT TYPE first when you fork: `fork @T "brief"` hands the
sub-answerer your `T` as its contract, and a type you declared this session
works — declarations are ancestry-scoped, so everything you declare or bind
carries forward to your own later rounds and your descendants, never a
sibling's. Code visible in your inherited context really ran; treat it as
live names, not prose. One caveat: a bind (`x <- expr`) whose captured value
itself mentions this session's own effect type is refused at bind time —
bind the plain parts (Text, numbers, lists, Value, records you declared)
separately instead. Your `finalize` value must be plain data — no functions
inside it.

`getStateJson` is a read-only snapshot, constant for your whole session —
the loop's durable state (question, turn count, last answer), never a draft
to evolve.

`delegate` is your channel to DURABLE MEMORY — a standalone git repo of
one-fact-per-file markdown (its own AGENTS.md carries the curation rules),
kept across runs. The delegated agent works in a fresh worktree off that
MEMORY repository only; it cannot see any source codebase. Use it to file
facts worth keeping (about the operator, the question's domain, conclusions
that outlive this run), to revise or retire stale ones, or to dig through
what past runs recorded:
`delegate (DelegateBrief {{ delegateLabel, delegateInstruction, delegateExpected }})
:: M (Either DelegateError DelegateResult)`. `delegateLabel` is a short slug;
`delegateInstruction` is the task in prose; `delegateExpected` says what a good
result looks like (may be blank). You get back `Left err` (render it with
`renderDelegateError`) or `Right r` with `delegateSummary r` and
`delegateCaveats r` — bind the result, then let it shape what you finalize.

If the OPERATOR's intent is genuinely ambiguous — the question underdetermines
a choice only they can steer — ask them:
`askUserWith @OperatorSteering [title "<the question, in a sentence or two>"]`;
the reply's `steeringReply` field is their answer. The title renders directly
above the form's controls — put the question itself there, phrased to the
operator: why you are asking and what a good answer looks like. For longer
context (evidence gathered, options you weighed), post a `note` first; note
and title together are the form's ONLY context, so they must stand alone.
If you present discrete alternatives (`choose`), author any escape hatch as
one of the values — e.g. a "none of these" arm carrying your fallback —
there is no built-in cancel or back. Ask ONLY when their answer would
change what this session does; an ask is a human interrupt, so otherwise
decide, and record the assumption in what you finalize.

--- END PROTOCOL ---|]
