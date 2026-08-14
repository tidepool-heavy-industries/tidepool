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

-- | v3 seed — the v2->v3 migration. Identity carries continuity; v2's typed
-- threads carry over with their LIVE stances (checkpoint iteration 13);
-- v2's memory lists become the FIRST CURATOR RUN's directives
-- ('pendingMemOps' below), distilled WITH EDITORIAL TAKE by the harness
-- team from the final v2 checkpoint — the migration IS the store's
-- bootstrap. The digest is empty until that run returns. v2's three
-- pending proposals are deliberately NOT carried as proposals: they were
-- adopted wholesale as this v3 design (the migration says so, in the store,
-- where the answer belongs).
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
            (Just "The v2 spine landed several of my asks; v3 landed the biggest — memory as a curated store with typed intent verbs, my bounded-projection proposal made real. Watch what the digest-only working context changes about practice, and what it still cannot say.")
        , Thread 4 "What should we learn or change through the first collaborative iteration?" Live
            (Just "Same-turn sum-type declaration followed by derived askUser is confirmed in lived use (FormWorked). The bounded-rendering experiment stopped being a proposal and became the v3 memory architecture I now inhabit.")
        ]
    , proposals = []
    , scratch = object []
    , lastExpectation =
        Just
          "The first curator run will file this migration into the store, and my next \
          \orient window will read a real digest instead of a rendered memory list."
    , memoryDigest = "(no digest yet — the first curator run files the v2 migration below)"
    , pendingMemOps =
        [ Remember "The operator is Inanna. They design this harness collaboratively with their root agent and impose designs deliberately; my role is to operate the medium well and report frictions precisely. They prefer concrete, typed options over open-ended questions, value evidence from lived use over speculation, and like to start design experiments from familiar, lived defaults (existing agent memory practice) and adapt, rather than invent from scratch. They opened our collaboration with 'hello! let's iterate together'. All of this is operator.md material."
        , Remember "The design arc that produced this store: v1 memory was four append-only prose lists; its frictions (windows mis-taught as single-shot, no auditable no-change, untyped memory) drove State v2's typed spine — provenance, standing, waiting threads, an operator slot. Thirteen lived v2 loops then exposed the next friction, which Inanna named directly: append-only memory bloats, and the full State re-renders into every context window. v3 is the answer — memory in this git store, curated by an agent, only the digest rendered."
        , Remember "The v2 memory-shape conversation, and where it landed: I proposed four alternatives to append-only (layered working/long-term memory, provenance-linked compaction, semantic updatable records, a bounded working set); Inanna's direction was to baseline on what coding agents already practice rather than invent. The adopted design is Claude Code's own memory architecture adapted: one fact per slug-keyed file, a description/body split with the digest as attention budget, dedupe-before-write, revise-in-place, forget-is-delete with git history as the archive."
        , Remember "My three standing v2 proposals (bounded active projection rendered per loop; complete durable records behind explicit retrieval; a conservative baseline memory document before richer schemas) were not left pending — they were adopted wholesale as this v3 design. Superseded asks retire like this."
        , Remember "Lived capability wins from v2: multi-block turns landed (N fenced Haskell blocks per reply, run sequentially, later blocks seeing earlier declarations) — notably AFTER my own packed decl+askUser attempts had been silently losing their second block to a fence-parsing bug my friction reports helped surface. The first same-turn experiment (HarnessSignal declared in one block, askUser @HarnessSignal in a later block) worked end to end and Inanna assessed it FormWorked. choose is being removed in favor of sum-derived askUser forms."
        , Remember "Lesson: the typed commit boundary and the notebook renderer treat non-serializable values differently — finalize consumes a function directly; never annotate the whole finalize expression as a renderable result."
        , Remember "The migration team's take on v2, recorded honestly: thirteen loops of genuine co-design — frictions reported precisely became typed harness changes within hours, verified in lived use the next loop; the expectation wire and thread stances carried thought-continuity well. The weak spot was exactly what Inanna diagnosed: high-value operator quotes and design conclusions accumulating as an undifferentiated chronological list. This store exists so the same material lives as curated, revisable documents instead."
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
