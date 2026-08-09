{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeOperators #-}

-- | The self-iterating harness's nested answerer Agent's capability
-- boundary — the LITERAL counterpart to 'Tidepool.Harness.HarnessEff' on the
-- other side of the harness\/agent structural split.
--
-- @type Agent = 'Eff' \'[AskUser, Fork, Finalize]@ is the row an answerer
-- turn compiles against: the answerer's compile passes ONLY the
-- @[askuser_decl, fork_decl, finalize_decl]@ effect declarations, so its
-- generated @Tidepool.Effects@ declares 'AskUser', 'Fork', and 'Finalize'
-- (and their Member-polymorphic verbs — @askUserRaw@\/@fork@\/@forkAll@\/
-- @finalize@) but never declares 'RunLLMTurn' at all (an effect's GADT +
-- helpers are only emitted for decls actually passed into a given compile).
-- An answerer turn answers via @finalize \@T@, gathers operator input with
-- 'Tidepool.Form.askUser' \/ 'Tidepool.Form.choose' \/
-- 'Tidepool.Form.chooseMany' (the only operator verbs in its row, all riding
-- 'AskUser'), and delegates to parallel sub-answerers with 'fork'\/'forkAll'
-- (riding 'Fork', 'Tidepool.Fork') — and NOTHING else.
--
-- Because @RunLLMTurn@ is never declared in this compile, a @runLLMTurn@
-- call inside an answerer turn is a COMPILE ERROR — GHC reports it "not in
-- scope", not a solvable-elsewhere type mismatch. That is the whole point:
-- "the answerer cannot recursively spawn models directly" is a property of
-- what its compile is given to work with, not of the prompt\/framing — it
-- can still spawn parallel sub-answerers, but only through 'fork'\/'forkAll',
-- which are typed, batched, and depth-one (a forked child answers its own
-- brief directly and cannot itself fork).
--
-- This module is importable by an answerer-stack compile (where 'AskUser',
-- 'Fork', and 'Finalize' are declared) as the literal spelling of that
-- boundary, mirroring 'Tidepool.Harness.HarnessEff' — an answerer turn's own
-- code (model-generated per turn, e.g. @finalize \@Decision (...) :: M ()@)
-- does not need to import it to be bound by the same row; @M@ already
-- resolves to it in that context.
module Tidepool.Agent
  ( Agent
  ) where

import Control.Monad.Freer (Eff)
import Tidepool.Effects (AskUser, Fork, Finalize)

-- | See the module haddock: the answerer turn's
-- @'Eff' \'[AskUser, Fork, Finalize]@ capability boundary, written out as a
-- concrete row.
type Agent = Eff '[AskUser, Fork, Finalize]
