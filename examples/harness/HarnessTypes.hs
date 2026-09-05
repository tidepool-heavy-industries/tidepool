{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | REFERENCE ARTIFACT (self-iterating-harness S3 scaffold; split out of
-- 'Harness' by the harness\/agent structural-split work). The author-facing
-- 'State'\/'Decision' vocabulary and the pure 'render' function, WITHOUT any
-- reference to 'Tidepool.Harness'\/@runLLMTurn@ — deliberately, so this
-- module compiles under ANY effect row, including the self-iterating
-- harness's nested answerer turn (@Eff '[Ask, Finalize]@, no @RunLLMTurn@).
--
-- 'State' carries no loop-iteration counter and 'render' takes no
-- compaction argument — those are runtime facts, composed by the driver
-- into the full system message alongside this module's output (see
-- @plans\/self-iterating-harness\/15-generic-surface-wave.md@, "Runtime
-- context is the runtime's job").
--
-- Why this is its own module rather than living in 'Harness' alongside
-- 'loop': the nested answerer imports these types (to build a typed
-- @finalize \@Decision (...)@ reply) but must NOT thereby pull in 'loop'
-- (which calls @runLLMTurn@ and so needs 'RunLLMTurn' in its compile's
-- effect row). GHC compiles a module as a whole — if 'loop' lived here, the
-- answerer's import would force this module through the answerer's
-- @Eff '[Ask, Finalize]@ compile, and 'loop'\'s @runLLMTurn@ call would fail
-- to resolve (its effect isn't even declared in that row). Splitting the
-- data\/render half out keeps the answerer's import surface genuinely
-- @RunLLMTurn@-free, so the harness\/agent boundary is enforced by which
-- decls each compile is given, not by a behavioral convention. 'Harness'
-- re-exports everything here (plus 'loop'), so the OUTER harness compile
-- (@Eff '[RunLLMTurn]@) sees the exact same contract it always has.
module HarnessTypes
  ( State (..)
  , Mode (..)
  , Decision (..)
  , Confidence (..)
  , initialState
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
-- `render` is this module's OWN export — hidden here since
-- `Tidepool.Prelude` also exports an unrelated `render` (`Tidepool.Render`).
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

-- | The author-defined 'State' this harness threads through 'loop' and reads
-- in 'render'. A single-constructor record, per the
-- @deriving (Generic, ToJSON, FromJSON)@ convention used throughout
-- @haskell/lib/Tidepool@ (structural, no Template Haskell). Note it stores a
-- typed 'Decision' ('lastDecision'), so a typed answer flows
-- @runLLMTurn -> State -> (serialized across the loop boundary) -> render@.
data State = State
  { mode         :: Mode
  , notes        :: [Text]
  , lastDecision :: Maybe Decision
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | An example small, typed State field — the "enum/level/mode" shape
-- 02-runtime.md calls out, not a free-form blob.
--
-- A nullary sum (every constructor has no fields — an enum) derives
-- 'ToJSON'\/'FromJSON' via 'GHC.Generics': Tidepool's vendored Aeson encodes
-- each constructor as its bare name string (@Observing -> "Observing"@) and
-- decodes back the same way.
data Mode = Observing | Deciding | Acting
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The typed answer 'Harness.loop' asks for via @runLLMTurn \@Decision@ —
-- NOT a bare 'Text'. This is the whole point of the shared-code model: the
-- RunLLMTurn / finalize machinery (WS-B) carries any monomorphic
-- @FromJSON a => a@ answer back through GHC-as-validator (an ill-typed
-- answer never consumes the continuation). Defined here alongside 'State'\/
-- 'Mode', and it NESTS ('Confidence') to show structured answers cross
-- whole, not just flat.
data Decision = Decision
  { action     :: Text        -- ^ the single next thing to do
  , rationale  :: Text        -- ^ why, in one sentence
  , confidence :: Confidence  -- ^ nested typed field — structured answers nest
  }
  deriving (Generic, ToJSON, FromJSON, Show)

-- | A nested typed field of 'Decision' — proves an ADT-within-an-ADT answer
-- round-trips through the typed yield. A nullary sum, so 'ToJSON'\/'FromJSON'
-- derive via 'GHC.Generics' the same way 'Mode' does above.
data Confidence = Low | Medium | High
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The runtime's very first loop starts from this 'State' (before any
-- persisted State exists to restore).
initialState :: State
initialState =
  State {mode = Observing, notes = [], lastDecision = Nothing}

-- | @render :: State -> Text@. Plain Haskell conditionals + the @[fmt|]@
-- quasiquoter over 'State' — no jinja, no effects: this function cannot
-- itself suspend or call 'Harness.loop'\'s @runLLMTurn@ (that's what makes
-- per-turn re-rendering unrepresentable by construction). Domain policy
-- only — the driver composes this output with the loop-iteration count, the
-- prior compaction summary, and capability/finalization instructions.
render :: State -> Text
render st =
  [fmt|You are a self-iterating agent, currently {modeLine}.
{lastDecisionBlock}
{notesBlock}|]
  where
    modeLine = case mode st of
      Observing -> "observing" :: Text
      Deciding -> "deciding what to do next"
      Acting -> "acting on a prior decision"
    lastDecisionBlock = case lastDecision st of
      Nothing -> "No decision made yet." :: Text
      Just d ->
        "Last decision: " <> action d
          <> " (confidence: " <> T.pack (show (confidence d)) <> ")"
    notesBlock
      | null (notes st) = "No notes carried forward yet."
      | otherwise =
          "Notes carried forward:\n"
            <> T.intercalate "\n" (map ("- " <>) (notes st))
