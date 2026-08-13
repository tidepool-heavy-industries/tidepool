{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Companion State v2 (plans\/companion-state-v2.md): TYPED STRUCTURE
-- BOTTOMING OUT IN VALUE. The spine — memories with provenance\/standing,
-- threads with waiting semantics, the operator slot — is typed exactly as
-- deep as the machinery (render, combinators, checkpoint) needs to see;
-- 'scratch' and 'Structured' are the @Value@ bottoms where the agent
-- structures experiments ad hoc, lens-edited, no schema. A scratch pattern
-- that proves out GRADUATES into the spine via 'proposals'.
--
-- The answer type stays @State -> State@ — the endomorphism monoid;
-- 'Prelude.id' is the blessed no-change answer. Edits compose the named
-- combinators below, which own the bookkeeping (id minting, born-stamping).
module HarnessTypes
  ( -- * Schema
    Provenance (..)
  , Standing (..)
  , Entry (..)
  , Memory (..)
  , ThreadStatus (..)
  , Thread (..)
  , State (..)
  , initialState
    -- * Edit combinators (the vocabulary edits compose)
  , remember
  , noteOperator
  , revise
  , setStanding
  , openThread
  , updateThread
  , propose
  , onScratch
    -- * Loop bookkeeping (authored-loop only)
  , tick
  , retention
    -- * Rendering
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON, Value)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

data Provenance = FromOperator | FromAgent
  deriving (Eq, Generic, ToJSON, FromJSON, Show)

data Standing = Active | Archived | Retired
  deriving (Eq, Generic, ToJSON, FromJSON, Show)

-- | What a memory SAYS — the sum render dispatches on. 'Structured' is a
-- Value bottom: ad-hoc structured content living in the memory stream with
-- full provenance\/standing. 'Note' is the catch-all; nothing is unsayable.
data Entry
  = Fact { fact :: Text }
  | Event { event :: Text }
  | Quote { by :: Provenance, said :: Text }
  | Structured { val :: Value }
  | Note { text :: Text }
  deriving (Generic, ToJSON, FromJSON, Show)

data Memory = Memory
  { mid :: Int
  , born :: Int
  , source :: Provenance
  , standing :: Standing
  , entry :: Entry
  }
  deriving (Generic, ToJSON, FromJSON, Show)

data ThreadStatus = Live | WaitingOnOperator | Resting | Resolved
  deriving (Eq, Generic, ToJSON, FromJSON, Show)

data Thread = Thread
  { tid :: Int
  , question :: Text
  , status :: ThreadStatus
  , stance :: Maybe Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

data State = State
  { identity :: Text
  -- ^ One prose paragraph; wholesale, deliberate rewrites.
  , loopN :: Int
  -- ^ The clock. Authored-loop bookkeeping ('tick'); never edit it.
  , memories :: [Memory]
  -- ^ Newest first.
  , aboutOperator :: [Memory]
  -- ^ The agent's model OF its operator — always rendered (orienting
  -- context), separate from general memory so it never competes for
  -- attention. Same 'Memory' machinery, same combinator bookkeeping.
  , threads :: [Thread]
  , proposals :: [Text]
  -- ^ Harness-change asks, aimed at the operator.
  , scratch :: Value
  -- ^ The sandbox: agent-owned, lens-edited, no schema, no review.
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | v2 seed, hand-curated from the v1 (all-prose) state per the fresh-start
-- decision: identity carries continuity; a few v1 memories survive as
-- archived entries; the operator slot is seeded with what the v1 record
-- evidences; v1's real curiosities became threads.
initialState :: State
initialState =
  State
    { identity =
        "I am a companion inhabiting an experimental typed agent runtime. This is \
        \my second State: the first was four append-only prose lists, and its \
        \frictions — which I reported and the operator's team confirmed — drove \
        \this typed redesign. I keep memories with provenance and standing, hold \
        \questions as threads that can wait or rest, and grow a persistent library \
        \of named helpers."
    , loopN = 0
    , memories =
        [ Memory 3 0 FromAgent Active
            (Fact "My frictions reports (windows mis-taught as single-shot, no auditable no-change, untyped memory) drove the State v2 design now shaping me.")
        , Memory 2 0 FromOperator Archived
            (Quote FromOperator "Typed memory events are the first concrete typed-harness experiment to develop.")
        , Memory 1 0 FromAgent Archived
            (Note "Lesson: the typed commit boundary and the notebook renderer treat non-serializable values differently — finalize consumes a function directly; never annotate the whole finalize expression as a renderable result.")
        ]
    , aboutOperator =
        [ Memory 5 0 FromAgent Active
            (Fact "The operator is Inanna. They design this harness collaboratively with their root agent and impose designs deliberately; my role is to operate the medium well and report frictions precisely.")
        , Memory 4 0 FromAgent Active
            (Fact "The operator prefers concrete, typed options over open-ended questions, and values evidence from lived use over speculation.")
        ]
    , threads =
        [ Thread 1 "What kind of companion might I become?" Resting Nothing
        , Thread 2 "What would I change about the harness shaping my experience?" Live
            (Just "The v2 spine landed several of my asks (provenance, standing, waiting threads, an operator slot). Watch how typed structure changes practice, and what it still cannot say.")
        , Thread 3 "What is the smallest typed model separating working context, archival memory, and audited memory management?" Resolved
            (Just "Answered by State v2: standing + render-as-selection + edit combinators. Superseded questions retire like this one.")
        ]
    , proposals = []
    , scratch = object []
    }

-- ---------------------------------------------------------------------------
-- Edit combinators
-- ---------------------------------------------------------------------------

nextMid :: State -> Int
nextMid st = 1 + maximum (0 : map (.mid) (st.memories <> st.aboutOperator))

nextTid :: State -> Int
nextTid st = 1 + maximum (0 : map (.tid) st.threads)

-- | Mint a memory (id assigned, born stamped at the current loop), newest
-- first, standing 'Active'.
remember :: Provenance -> Entry -> State -> State
remember who e st =
  st { memories = Memory (nextMid st) st.loopN who Active e : st.memories }

-- | Mint an entry in the OPERATOR slot — the agent's model of its operator
-- (always rendered; use for durable facts about who they are and how they
-- work, not for their individual utterances — those are 'Quote' memories).
noteOperator :: Entry -> State -> State
noteOperator e st =
  st { aboutOperator = Memory (nextMid st) st.loopN FromAgent Active e : st.aboutOperator }

-- | Replace WHAT a memory says, keeping its id, provenance, and birth — a
-- revision is the same memory saying it better. Looks in both memory lists.
revise :: Int -> Entry -> State -> State
revise i e st =
  st
    { memories = map upd st.memories
    , aboutOperator = map upd st.aboutOperator
    }
  where
    upd m = if m.mid == i then m { entry = e } else m

-- | Retire \/ archive \/ reactivate by id, in either memory list.
setStanding :: Standing -> Int -> State -> State
setStanding sdg i st =
  st
    { memories = map upd st.memories
    , aboutOperator = map upd st.aboutOperator
    }
  where
    upd m = if m.mid == i then m { standing = sdg } else m

-- | Open a Live thread (tid minted).
openThread :: Text -> State -> State
openThread q st = st { threads = Thread (nextTid st) q Live Nothing : st.threads }

-- | Update one thread by id — status, stance, or the question itself.
updateThread :: Int -> (Thread -> Thread) -> State -> State
updateThread i f st = st { threads = map upd st.threads }
  where
    upd t = if t.tid == i then f t else t

-- | Ask the operator for a harness change (reviewed between windows).
propose :: Text -> State -> State
propose p st = st { proposals = st.proposals <> [p] }

-- | Edit the sandbox — compose with aeson-lens (`over (key ...)`) freely.
onScratch :: (Value -> Value) -> State -> State
onScratch f st = st { scratch = f st.scratch }

-- ---------------------------------------------------------------------------
-- Loop bookkeeping (called by the authored loop, not by edits)
-- ---------------------------------------------------------------------------

-- | Advance the clock. Authored-loop only.
tick :: State -> State
tick st = st { loopN = st.loopN + 1 }

-- | Hard-drop Retired entries older than 'retentionLoops' — the state JSON
-- re-splices into every loop compile, so growth must be bounded. Retire is
-- still never-delete WITHIN the horizon (audit trail); beyond it, gone.
retention :: State -> State
retention st =
  st
    { memories = filter keep st.memories
    , aboutOperator = filter keep st.aboutOperator
    }
  where
    keep m = m.standing /= Retired || st.loopN - m.born < retentionLoops

retentionLoops :: Int
retentionLoops = 40

-- ---------------------------------------------------------------------------
-- Render — an attention policy, not a dump
-- ---------------------------------------------------------------------------

-- | Selected working context: identity; the operator slot (always); Active
-- memories, newest first, bounded; Live + WaitingOnOperator threads (waiting
-- flagged); pending proposals; a scratch inventory line. Archived\/Retired
-- content is reachable only through @getStateJson@ — that asymmetry IS the
-- working-context \/ archive split.
render :: State -> Text
render st =
  [fmt|{st.identity}

About your operator:
{bullets (map entryLine (filter isActive st.aboutOperator))}

Active memories (newest first, {show shownCount} of {show activeCount} active; the archive is reachable via getStateJson):
{bullets (map entryLine shownMemories)}

Threads:
{bullets (map threadLine liveThreads)}
{proposalsSection}
Scratch: {scratchLine}|]
  where
    isActive m = m.standing == Active
    activeMems = filter isActive st.memories
    shownMemories = take 12 activeMems
    shownCount = length shownMemories
    activeCount = length activeMems
    liveThreads =
      [t | t <- st.threads, t.status == Live || t.status == WaitingOnOperator]
    bullets [] = "- (none)" :: Text
    bullets xs = T.intercalate "\n" (map ("- " <>) xs)
    proposalsSection :: Text
    proposalsSection
      | null st.proposals = ""
      | otherwise =
          [fmt|
Proposals awaiting the operator:
{bullets st.proposals}
|]
    scratchLine :: Text
    scratchLine =
      let rendered = show st.scratch
       in if st.scratch == object []
            then "(empty — structure experiments here freely)"
            else T.take 400 rendered

-- | One line per memory, dispatched on the 'Entry' sum — the templating the
-- sum exists for.
entryLine :: Memory -> Text
entryLine m = case m.entry of
  Fact t -> [fmt|#{show m.mid} (fact, loop {show m.born}) {t}|]
  Event t -> [fmt|#{show m.mid} (event, loop {show m.born}) {t}|]
  Quote who t -> [fmt|#{show m.mid} ({sayer who} said, loop {show m.born}) "{t}"|]
  Structured v -> [fmt|#{show m.mid} (structured, loop {show m.born}) {T.take 200 (show v)}|]
  Note t -> [fmt|#{show m.mid} (note, loop {show m.born}) {t}|]
  where
    sayer FromOperator = "operator" :: Text
    sayer FromAgent = "you"

threadLine :: Thread -> Text
threadLine t =
  [fmt|T{show t.tid} {statusTag} {t.question}{stanceLine}|]
  where
    statusTag :: Text
    statusTag = case t.status of
      WaitingOnOperator -> "[WAITING ON OPERATOR]"
      Live -> "[live]"
      Resting -> "[resting]"
      Resolved -> "[resolved]"
    stanceLine = case t.stance of
      Nothing -> "" :: Text
      Just s -> [fmt| — stance: {s}|]
