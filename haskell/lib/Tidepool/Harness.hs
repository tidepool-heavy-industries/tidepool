{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The self-iterating harness's own orchestration monad
-- (self-iterating-harness WS-B, @plans\/self-iterating-harness\/07-impl-orchestration.md@):
-- an authored module (see @examples\/harness\/Harness.hs@, the target
-- contract) writes @render :: State -> Text@ and @loop ::
-- State -> Harness State@ against 'Harness', driven by the
-- @tidepool-selfharness@ runtime.
--
-- 'Harness' is simply @M@ (the compiled program's OWN effect stack) under a
-- friendlier name, NOT a hardcoded @Eff '[RunLLMTurn]@ — the outer
-- Harness-monad session the self-iterating-harness driver bootstraps
-- compiles its @Tidepool.Effects@ against exactly the @RunLLMTurn@-only decl
-- list (@tidepool_mcp::runllmturn_decl()@, no base effects — v1's LOCKED
-- scope, base effects appended later, as needed), so @M@ — and therefore
-- 'Harness' — resolves to @Eff '[RunLLMTurn]@ in THAT context.
--
-- This module is imported ONLY from that one context in practice
-- (@examples\/harness\/Harness.hs@'s @loop@ and the analogous test
-- fixtures) — the self-iterating harness's nested ANSWERER turn never
-- imports it: its own compile excludes @RunLLMTurn@ entirely (see
-- @tidepool_harness::selfharness::driver::answerer_decls@ and
-- 'Tidepool.Agent', the answerer's own capability-boundary module). A
-- general (non-self-harness) Agent turn's compile (base effects + @Ask@ +
-- @RunLLMTurn@ + @Finalize@, @tidepool_harness::engine@'s private
-- @agent_decls@) is a THIRD, unrelated context this module happens to also
-- work against unmodified, since it never names the effect list itself —
-- only 'HarnessEff' does.
module Tidepool.Harness
  ( Harness
  , HarnessEff
  , runLLMTurn
  ) where

import Control.Monad.Freer (Eff)
-- 'runLLMTurn' is RE-EXPORTED verbatim from 'Tidepool.Effects' (see below) —
-- NOT wrapped. A local wrapper @runLLMTurn = Effects.runLLMTurn@ would compile
-- once as a polymorphic binding, so the extract's typed-yield site-id pass sees
-- its INTERNAL @Effects.runLLMTurn \@a@ call at a bare type VARIABLE and rejects
-- it ("polymorphic runLLMTurn site"). A re-export has no such internal site: a
-- call @runLLMTurn \@Text@ IS @Effects.runLLMTurn \@Text@ directly, concrete.
import Tidepool.Effects (M, RunLLMTurn, runLLMTurn)

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
-- DIVERGE everywhere else: the nested ANSWERER turn's compile
-- (@Eff \'[Ask, Finalize]@, see 'Tidepool.Agent') EXCLUDES @RunLLMTurn@
-- entirely and never imports this module at all; a general (non-self-harness)
-- Agent turn's compile (the full @EngineConfig::standard@ stack) resolves
-- @M@ — and therefore 'Harness' — to a DIFFERENT, wider effect row, while
-- 'HarnessEff' stays pinned to @\'[RunLLMTurn]@. This module works unmodified
-- against that general-Agent context because its verbs are written in terms
-- of 'M' ('Harness'), never the literal row; reach for 'HarnessEff' ONLY at a
-- decl site that genuinely means the outer @\'[RunLLMTurn]@ boundary and must
-- NOT follow @M@ as base effects are appended.
type HarnessEff = Eff '[RunLLMTurn]

-- 'runLLMTurn' — suspend 'loop' for a TYPED answer (@runLLMTurn \@T prompt@):
-- the driver answers by driving a nested Agent turn loop (a fresh multi-turn
-- sub-session over the SAME calling model) to a @finalize@
-- (self-iterating-harness WS-A/B), whose value resumes this hole. GHC validates
-- the answer against @T@ before it resumes the continuation, so an ill-typed
-- answer never consumes it. Re-exported straight from 'Tidepool.Effects' (in the
-- import above) to match the @examples\/harness\/Harness.hs@ target contract's
-- @import Tidepool.Harness (Harness, runLLMTurn)@ — see the import haddock for
-- why it is a re-export and not a wrapper.
