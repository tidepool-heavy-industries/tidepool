{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeOperators #-}

-- | The self-iterating harness's nested answerer Agent's capability
-- boundary — the LITERAL counterpart to 'Tidepool.Harness.HarnessEff' on the
-- other side of the harness\/agent structural split.
--
-- @type Agent = 'Eff' \'[Ask, Finalize]@ is the row an answerer turn compiles
-- against: 'tidepool_harness::selfharness::driver::answerer_decls' passes
-- ONLY @[ask_decl, finalize_decl]@ to that turn's compile, so its generated
-- @Tidepool.Effects@ declares 'Ask' and 'Finalize' (and their Member-
-- polymorphic verbs — @ask@\/@dialogAsk@\/@finalize@) but never declares
-- 'RunLLMTurn' at all (`tidepool_mcp::effects_module_source` only emits an
-- effect's GADT + helpers for decls actually passed into a given compile).
-- An answerer turn answers a @runLLMTurn@ hole with @finalize \@T@ and
-- gathers operator input with @dialogForm@\/@dialogAsk@ (both ride 'Ask') —
-- and NOTHING else.
--
-- Because @RunLLMTurn@ is never declared in this compile, a @runLLMTurn@
-- call inside an answerer turn is a COMPILE ERROR — GHC reports it "not in
-- scope", not a solvable-elsewhere type mismatch. That is the whole point:
-- "the answerer cannot recursively spawn models" is a property of what its
-- compile is given to work with, not of the prompt\/framing (which was W1's
-- interim scoping, before the answer types were split out of the module
-- that also defines @loop@ — see @examples\/harness\/HarnessTypes.hs@'s
-- haddock for why that split was the remaining blocker).
--
-- This module is importable by an answerer-stack compile (where 'Ask' and
-- 'Finalize' are declared) as the literal spelling of that boundary,
-- mirroring 'Tidepool.Harness.HarnessEff' — an answerer turn's own code
-- (model-generated per turn, e.g. @finalize \@Decision (...) :: M ()@) does
-- not need to import it to be bound by the same row; @M@ already resolves to
-- it in that context.
module Tidepool.Agent
  ( Agent
  ) where

import Control.Monad.Freer (Eff)
import Tidepool.Effects (Ask, Finalize)

-- | See the module haddock: the answerer turn's @'Eff' \'[Ask, Finalize]@
-- capability boundary, written out as a concrete row.
type Agent = Eff '[Ask, Finalize]
