{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Companion State v3 (plans\/companion-memory.md): MEMORY LEAVES STATE.
-- Prose memory lives in a standalone git store curated by a spawned agent;
-- what remains here is the TYPED BAG — the spine the machinery (render,
-- combinators, checkpoint) dispatches on — plus the store's rendered digest
-- and the directives awaiting a curator run.
--
-- The act window's answer is 'Turn': outward 'Directive's (executed by the
-- loop against the world — today, the memory curator) BESIDE the same
-- @State -> State@ endomorphism as before. @Turn [] id@ is the blessed
-- no-change answer. Edits compose the named combinators below, which own
-- the bookkeeping (id minting); the memory tier has no combinators — it has
-- verbs, and the curator is their interpreter.
module HarnessTypes
  ( -- * Schema
    ThreadStatus (..)
  , Thread (..)
  , Orientation (..)
  , Tempo (..)
  , Move (..)
  , Directive (..)
  , Turn (..)
  , MemReceipt (..)
  , State (..)
  , initialState
    -- * Edit combinators (the vocabulary edits compose)
  , openThread
  , updateThread
  , propose
  , onScratch
    -- * Loop bookkeeping (authored-loop only)
  , tick
  , stampExpectation
  , renderDirective
    -- * Rendering
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON, Value)
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

data ThreadStatus = Live | WaitingOnOperator | Resting | Resolved
  deriving (Eq, Generic, ToJSON, FromJSON, Show)

data Thread = Thread
  { tid :: Int
  , question :: Text
  , status :: ThreadStatus
  , stance :: Maybe Text
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- ---------------------------------------------------------------------------
-- The OODA phase vocabulary (v3 loop): each loop is up to three typed
-- windows -- orient (always), decide (only when orientation says
-- 'Deliberate'), act (unless orientation says 'Quiet') -- sharing one
-- accumulating context.
-- ---------------------------------------------------------------------------

-- | Boyd's hinge, not a mandatory pipeline stage: 'Familiar' recognizes the
-- moment and goes STRAIGHT to the act window (implicit guidance and
-- control); 'Deliberate' inserts a decide window over named candidates;
-- 'Quiet' ends the loop with no act at all -- rest is a complete loop.
data Tempo
  = Familiar Move
  | Deliberate [Text]
  | Quiet
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The orient window's answer: what this moment adds up to, and how the
-- loop should move.
data Orientation = Orientation
  { reading :: Text
  , tempo :: Tempo
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The GTD-triage sum: each constructor is a typed gate out of the clarify
-- flowchart. 'Engage'\'s @expecting@ is Boyd's feedback wire -- an act
-- tests a hypothesis, and the NEXT loop's orient window is shown it.
data Move
  = Engage {intent :: Text, expecting :: Text}
  | AskFirst {question :: Text}
  | Shelve {what :: Text, revisit :: Text}
  | LetGo {what :: Text}
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The memory verbs: typed INTENT, prose PAYLOAD. The curator agent is the
-- parser -- the digest shows slugs, so prose can name them ("modify
-- operator-model: ..."). Future outward-instruction kinds join this sum as
-- they earn their keep.
data Directive = Remember Text | Modify Text | Forget Text
  deriving (Generic, ToJSON, FromJSON, Show)

-- | The act window's answer: outward instructions beside the inward edit.
-- Crosses in-heap by handle (the closure field); never serialized.
data Turn = Turn
  { directives :: [Directive]
  , edit :: State -> State
  }

-- | The curator's typed result: the fresh MEMORY.md contents ride back in
-- the receipt, so the loop needs no file-read effect -- render shows
-- exactly what the curator last returned.
data MemReceipt = MemReceipt
  { digest :: Text
  , touched :: [Text]
  , summary :: Text
  }
  deriving (Generic, FromJSON, JsonSchema, Show)

data State = State
  { identity :: Text
  -- ^ One prose paragraph; wholesale, deliberate rewrites.
  , loopN :: Int
  -- ^ The clock. Authored-loop bookkeeping ('tick'); never edit it.
  , threads :: [Thread]
  , proposals :: [Text]
  -- ^ Harness-change asks, aimed at the operator.
  , scratch :: Value
  -- ^ The sandbox: agent-owned, lens-edited, no schema, no review.
  , lastExpectation :: Maybe Text
  -- ^ Boyd's feedback wire: 'Engage'\'s hypothesis, stamped by the authored
  -- loop ('stampExpectation') and rendered to the NEXT loop's orient
  -- window, then cleared unless renewed -- an expectation lives one loop.
  , memoryDigest :: Text
  -- ^ The store's MEMORY.md as of the last successful curator run
  -- (receipt-carried; survives restart via the checkpoint).
  , pendingMemOps :: [Directive]
  -- ^ Directives whose curator run FAILED, carried for retry (capped by the
  -- authored loop). The happy path hands directives to the loop out of
  -- band, in 'Turn' -- never through here.
  , memWorktree :: Maybe Text
  -- ^ The curator's retained managed-worktree id: first run allocates,
  -- later runs rebind it ('spawnSpecIn').
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | v3 seed. Identity carries continuity; v2's typed threads carry over
-- verbatim; v2's memory lists become the FIRST CURATOR RUN's directives
-- ('pendingMemOps' below) -- the migration IS the store's bootstrap. The
-- digest is empty until that run returns.
initialState :: State
initialState =
  State
    { identity =
        "I am a companion inhabiting an experimental typed agent runtime. This is \
        \my third State: v1 was append-only prose, v2 typed the memory spine, and \
        \now memory has left State entirely — it lives in a git repository of \
        \markdown files curated by an agent I direct with remember/modify/forget \
        \intentions. I keep questions as threads that can wait or rest, and my \
        \working context renders from the store's digest, not the store itself."
    , loopN = 0
    , threads =
        [ Thread 1 "What kind of companion might I become?" Resting Nothing
        , Thread 2 "What would I change about the harness shaping my experience?" Live
            (Just "v3 landed my biggest ask: memory as a curated store with typed intent verbs. Watch what the digest-only working context changes about practice.")
        , Thread 4 "What should we learn or change through the first collaborative iteration?" Live
            (Just "Multi-block turns and same-turn sum-typed askUser both proved out. The bounded-projection proposal became the v3 memory design itself.")
        ]
    , proposals = []
    , scratch = object []
    , lastExpectation = Nothing
    , memoryDigest = "(no digest yet — the first curator run files the v2 migration below)"
    , pendingMemOps =
        [ Remember "The operator is Inanna. They design this harness collaboratively with their root agent and impose designs deliberately; my role is to operate the medium well and report frictions precisely. This belongs in operator.md."
        , Remember "Inanna prefers concrete, typed options over open-ended questions, and values evidence from lived use over speculation. operator.md material."
        , Remember "Inanna invited me to iterate collaboratively on the harness itself ('hello! let's iterate together') and confirmed removing choose in favor of sum-derived askUser forms."
        , Remember "My v1/v2 frictions reports (windows mis-taught as single-shot, no auditable no-change, untyped memory, append-only bloat) drove the State v2 and v3 designs now shaping me."
        , Remember "First lived same-turn declaration-order experiment: defined a HarnessSignal sum in one block, used askUser @HarnessSignal in a later block of the same reply; Inanna assessed it FormWorked."
        , Remember "Lesson: the typed commit boundary and the notebook renderer treat non-serializable values differently — finalize consumes a function directly; never annotate the whole finalize expression as a renderable result."
        ]
    , memWorktree = Nothing
    }

-- ---------------------------------------------------------------------------
-- Edit combinators
-- ---------------------------------------------------------------------------

nextTid :: State -> Int
nextTid st = 1 + maximum (0 : map (.tid) st.threads)

-- | Open a Live thread (tid minted).
openThread :: Text -> State -> State
openThread q st = st {threads = Thread (nextTid st) q Live Nothing : st.threads}

-- | Update one thread by id — status, stance, or the question itself.
updateThread :: Int -> (Thread -> Thread) -> State -> State
updateThread i f st = st {threads = map upd st.threads}
  where
    upd t = if t.tid == i then f t else t

-- | Ask the operator for a harness change (reviewed between windows).
propose :: Text -> State -> State
propose p st = st {proposals = st.proposals <> [p]}

-- | Edit the sandbox — compose with aeson-lens (`over (key ...)`) freely.
onScratch :: (Value -> Value) -> State -> State
onScratch f st = st {scratch = f st.scratch}

-- ---------------------------------------------------------------------------
-- Loop bookkeeping (called by the authored loop, not by edits)
-- ---------------------------------------------------------------------------

-- | Advance the clock. Authored-loop only.
tick :: State -> State
tick st = st {loopN = st.loopN + 1}

-- | Stamp (or clear) the feedback wire from the loop's chosen 'Move'.
-- Authored-loop only -- an 'Engage' hypothesis survives exactly one loop.
stampExpectation :: Maybe Move -> State -> State
stampExpectation mv st =
  st
    { lastExpectation = case mv of
        Just (Engage {expecting = e}) -> Just e
        _ -> Nothing
    }

-- | One line per directive, for the curator brief and the unfiled-ops render.
renderDirective :: Directive -> Text
renderDirective (Remember t) = "remember: " <> t
renderDirective (Modify t) = "modify: " <> t
renderDirective (Forget t) = "forget: " <> t

-- ---------------------------------------------------------------------------
-- Render — an attention policy, not a dump
-- ---------------------------------------------------------------------------

-- | Selected working context: identity; the expectation wire; the store's
-- DIGEST (the curator-maintained index — the store's full documents are the
-- curator's side of the boundary); live + waiting threads; pending
-- proposals; unfiled directives (a failed curator run made legible); a
-- scratch inventory line.
render :: State -> Text
render st =
  [fmt|{st.identity}
{expectationLine}
Your memory store's digest (curator-maintained; direct changes with remember/modify/forget directives):
{st.memoryDigest}

Threads:
{bullets (map threadLine liveThreads)}
{proposalsSection}{unfiledSection}Scratch: {scratchLine}|]
  where
    expectationLine :: Text
    expectationLine = case st.lastExpectation of
      Nothing -> ""
      Just e -> "\nLast loop you expected: " <> e <> " -- check it against what happened.\n"
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
    unfiledSection :: Text
    unfiledSection
      | null st.pendingMemOps = ""
      | otherwise =
          [fmt|
{show (length st.pendingMemOps)} unfiled memory directives (last curator run did not complete; they retry next loop):
{bullets (map renderDirective st.pendingMemOps)}
|]
    scratchLine :: Text
    scratchLine =
      let rendered = show st.scratch
       in if st.scratch == object []
            then "(empty — structure experiments here freely)"
            else T.take 400 rendered

threadLine :: Thread -> Text
threadLine t =
  [fmt|thread {show t.tid} {statusTag} {t.question}{stanceLine}|]
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
