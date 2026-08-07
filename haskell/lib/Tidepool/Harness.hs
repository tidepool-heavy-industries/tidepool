{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The self-iterating harness's own orchestration monad
-- (self-iterating-harness WS-B, @plans\/self-iterating-harness\/07-impl-orchestration.md@):
-- an authored module (see @examples\/harness\/Harness.hs@, the target
-- contract) writes @render :: State -> Maybe Text -> Text@ and @loop ::
-- State -> Harness State@ against 'Harness', driven by the
-- @tidepool-selfharness@ runtime.
--
-- 'Harness' is simply @M@ (the compiled program's OWN effect stack) under a
-- friendlier name, NOT a hardcoded @Eff '[RunLLMTurn]@ — the outer
-- Harness-monad session the self-iterating-harness driver bootstraps
-- compiles its @Tidepool.Effects@ against exactly the @RunLLMTurn@-only decl
-- list (@tidepool_mcp::runllmturn_decl()@, no base effects — v1's LOCKED
-- scope, base effects appended later, as needed), so @M@ — and therefore
-- 'Harness' — resolves to @Eff '[RunLLMTurn]@ in THAT context. An Agent
-- turn's own compile (base effects + @Ask@ + @RunLLMTurn@ + @Finalize@) sees
-- a different @M@; this module works unmodified against either, because it
-- never names the effect list itself.
module Tidepool.Harness
  ( Harness
  , HarnessEff
  , runLLMTurn
  , runLLMTurnWithRequiredResp
  ) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Tidepool.Effects as Effects
import Tidepool.Effects (M, RunLLMTurn)

-- | The self-iterating harness's orchestration monad — see the module
-- haddock for why this is an alias for @M@, not a literal effect list.
type Harness = M

-- | The LITERAL capability-boundary spelling of the OUTER Harness-monad
-- session: @'Eff' \'[RunLLMTurn]@, written out rather than resolved through
-- 'M'\'s context-dependent alias.
--
-- LOAD-BEARING (H1): 'HarnessEff' and 'Harness' (@= 'M'@) name the SAME type
-- in ONE context only — the self-iterating harness driver's OUTER session
-- (@tidepool_mcp::runllmturn_decl()@-only, so @M = Eff \'[RunLLMTurn]@). They
-- DIVERGE everywhere else: an Agent turn's compile (the nested answerer:
-- @[Ask, RunLLMTurn, Finalize]@, or the full @EngineConfig::standard@ stack)
-- resolves @M@ — and therefore 'Harness' — to a DIFFERENT, wider effect row,
-- while 'HarnessEff' stays pinned to @\'[RunLLMTurn]@. This module works
-- unmodified against either because its verbs are written in terms of 'M'
-- ('Harness'), never the literal row; reach for 'HarnessEff' ONLY at a decl
-- site that genuinely means the outer @\'[RunLLMTurn]@ boundary and must NOT
-- follow @M@ as base effects are appended.
type HarnessEff = Eff '[RunLLMTurn]

-- | Suspend 'loop' for a TYPED answer (@runLLMTurn \@T prompt@): the driver
-- answers by driving a nested Agent turn loop (a fresh multi-turn
-- sub-session over the SAME calling model) to a @finalize@
-- (self-iterating-harness WS-A/B), whose value resumes this hole. GHC
-- validates the answer against @T@ before it resumes the continuation — an
-- ill-typed answer never consumes it. Re-exports
-- 'Tidepool.Effects.runLLMTurn' under this module's own name, matching the
-- @examples\/harness\/Harness.hs@ target contract's
-- @import Tidepool.Harness (Harness, runLLMTurn)@.
runLLMTurn :: forall a. Text -> Harness a
runLLMTurn = Effects.runLLMTurn

-- | Alias for 'runLLMTurn' — the "required response" framing
-- (07-impl-orchestration.md\/03-agent-surface.md's proposed shape,
-- @runLLMTurnWithRequiredResp :: ... -> Harness a@) names the SAME verb: v1
-- has no separate "no required response" variant (that would mirror
-- @Tidepool.Effects.runLLMTurn :: forall a. Text -> M ()@ instantiated at
-- @()@, unused today), so both names resolve identically until one is
-- needed.
runLLMTurnWithRequiredResp :: forall a. Text -> Harness a
runLLMTurnWithRequiredResp = Effects.runLLMTurn
