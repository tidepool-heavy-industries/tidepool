{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | The recursive companion, collapsed around @fork@ (fork-subsumes-split
-- step 4): one turn is ONE top-level typed request.  The session it opens
-- decomposes the question by forking typed sub-answerers of its own —
-- @async (fork \@T brief)@, recursively, each a full multi-round session —
-- and the DRIVER services that tree: spawn-time depth\/descendant budgets,
-- operator-page node lifecycle, journal receipts.  The fold is ordinary
-- Haskell in the session's own block: the code after the waits.
--
-- Nothing recursive lives here anymore.  The authored loop's whole job is
-- the seed question, the one request, and the durable 'State' the answer
-- lands in.  Every session's answer type is exactly the @\@Type@ at its
-- invocation site — the root's is 'Text' because THIS call site starts
-- plain (operator decision, 2026-08-23; author-evolvable, same rule at
-- every level).
--
-- @Companion@ is @M@ at the driver's outer row (@RunLLMTurn@, @AskUser@,
-- @Console@, @Worktree@, @RepoEvent@, @Exec@, @Subagent@, and @Journal@ —
-- exactly the driver's widened outer session,
-- @selfharness::driver::outer_decls@, the same row @dev-tree@ compiles
-- against); this file declares no new effect. @tidepool-harness
-- \/tests\/dogfood_harness_typecheck.rs@ pins it against that row.
module Harness
  ( -- * The locked entry points
    State (..)
  , initialState
  , render
  , loop
  , resumeLoop

    -- * The driver's own vocabulary
  , Companion
  , rootPrompt
  ) where

import qualified Data.Text as T
import HarnessTypes
import Tidepool.Aeson (object, (.=))
import Tidepool.Effects (say)
import Tidepool.Form (askUser)
import Tidepool.Harness (Harness, runLLMTurn)
import Tidepool.Journal (record)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)
import Tidepool.Resume (ResumeFold)

-- | The orchestration monad.  @Harness@ is @M@ under a friendlier name; the
-- row it resolves to is the driver's outer session.
type Companion = Harness

-- | One resident turn: ask the ROOT session for the answer, store it.
--
-- The request is plain @runLLMTurn \@Text@ — in-context on the driver's
-- per-turn answerer node, whose framing IS 'render'\'s output, so the
-- session already holds the question, the last answer, and the protocol
-- before its first round.  Round exhaustion escalates through the driver's
-- own ladder (nudge, then the operator's allocate\/abort form) rather than
-- returning as data here — with ONE request per turn there is no sibling
-- work a hard exit could erase.
loop :: State -> Companion State
loop st
  -- The SEED GATE (operator decision, 2026-08-19): an empty question means
  -- no operator has chosen one yet, so the loop's first act is to ask —
  -- BEFORE any model session runs.  This is deliberately a SHORT cycle
  -- (ask, store, recurse): the answerer framing for a cycle is rendered
  -- from the state the cycle STARTED with, so running the request in the
  -- same cycle would run it under a framing whose question is still blank.
  -- The seeded question is 'State', so it checkpoints, and every later
  -- loop skips straight past this guard.  A blank submission re-asks
  -- (bounded by the driver's consecutive-re-presentation cap).
  | T.strip st.question == "" = do
      say "No question is seeded yet — provide the question this run should investigate."
      sq <- askUser @SeedQuestion
      case T.strip sq.seedQuestion of
        "" -> loop st
        q -> do
          say
            [fmt|Question seeded: {q}
Starting the first turn.|]
          -- RECURSE, don't return: seeding IS the operator's "go" — ending
          -- the cycle here would park them on a between-turns gate that
          -- asks them to confirm the thing they just did (dogfood finding,
          -- 2026-08-20). The price is that the seed only checkpoints once
          -- turn 1 completes, so a mid-turn crash re-asks the question —
          -- one cheap re-type against one pointless click per fresh run.
          loop st {question = q}
loop st = do
  record "turn" "root" (object ["question" .= st.question, "turn" .= turn])
  answer <- runLLMTurn @Text (rootPrompt st)
  record "turn" "root" (object ["answer" .= answer, "turn" .= turn])
  -- The turn's outcome, ON the operator page (dogfood finding, 2026-08-19):
  -- without this the parked between-turns screen says only "loop complete" —
  -- the operator sat 37 minutes next to a finished answer they couldn't see.
  say
    [fmt|Turn {show turn} complete.

{answer}

Start the next turn when ready — optionally with steering.|]
  pure st {turnCount = turn, lastAnswer = Just answer}
  where
    turn = st.turnCount + 1

-- | The honest opt-out ('Tidepool.Resume' module doc): this harness's
-- 'record' calls exist for the durable transcript, not to replay prior
-- sessions on a resumed boot — a rerun re-derives 'lastAnswer' from
-- 'State' the same way a fresh run does, so there is nothing here for a
-- fold of recorded steps to inject.  Declaring this (rather than leaving it
-- absent) is what turns a journal-bearing crash recovery from a boot
-- refusal into an ordinary 'loop' call.
resumeLoop :: ResumeFold -> State -> Companion State
resumeLoop _fold = loop

-- | The root request.  Exported for the slice test's scripted provider,
-- which keys on the @ROOT — turn@ header the same way it keys on any other
-- stable needle.  The mechanics (fork\/delegate\/askUser, the multi-round
-- rhythm) are taught by the framing — 'HarnessTypes.render' plus the
-- driver's own answerer suffix — so this says only what is per-request:
-- which turn this is, and what to finalize.
rootPrompt :: State -> Text
rootPrompt st =
  [fmt|ROOT — turn {show (st.turnCount + 1)}.

Question: {st.question}

Work this question with the full rhythm your framing describes — fork typed
sub-answerers for the lines of thought it opens, delegate repository work
where evidence or edits are needed — and finalize when another round would
not improve it.

Finalize: `finalize @Text (...)` — the answer as prose, written to be read
on its own.|]
