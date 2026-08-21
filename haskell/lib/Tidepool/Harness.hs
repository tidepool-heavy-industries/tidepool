{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The self-iterating harness's own orchestration monad: an authored module
-- (see @examples\/harness\/Harness.hs@, the target contract) writes
-- @render :: State -> Text@ and @loop :: State -> Harness State@ against
-- 'Harness', driven by the @tidepool-selfharness@ runtime.
--
-- 'Harness' is simply @M@ (the compiled program's OWN effect stack) under a
-- friendlier name, NOT a hardcoded @Eff '[RunLLMTurn]@ — the outer
-- Harness-monad session the self-iterating harness driver bootstraps
-- compiles its @Tidepool.Effects@ against exactly the @RunLLMTurn@-only decl
-- list, so @M@ — and therefore 'Harness' — resolves to @Eff '[RunLLMTurn]@ in
-- THAT context. This module is imported ONLY from that one context in
-- practice (@examples\/harness\/Harness.hs@'s @loop@ and the analogous test
-- fixtures).
module Tidepool.Harness
  ( Harness
  , runLLMTurn
  ) where

-- 'runLLMTurn' is RE-EXPORTED verbatim from 'Tidepool.Effects' (see below) —
-- NOT wrapped. A local wrapper @runLLMTurn = Effects.runLLMTurn@ would compile
-- once as a polymorphic binding, so the extract's typed-yield site-id pass sees
-- its INTERNAL @Effects.runLLMTurn \@a@ call at a bare type VARIABLE and rejects
-- it ("polymorphic runLLMTurn site"). A re-export has no such internal site: a
-- call @runLLMTurn \@Text@ IS @Effects.runLLMTurn \@Text@ directly, concrete.
import Tidepool.Effects (M, runLLMTurn)

-- | The self-iterating harness's orchestration monad — see the module
-- haddock for why this is an alias for @M@, not a literal effect list.
type Harness = M

-- 'runLLMTurn' — suspend 'loop' for a TYPED answer (@runLLMTurn \@T prompt@):
-- the driver answers by driving a nested Agent turn loop (a fresh multi-turn
-- sub-session over the SAME calling model) to a @finalize@
-- (self-iterating-harness WS-A/B), whose value resumes this hole. GHC validates
-- the answer against @T@ before it resumes the continuation, so an ill-typed
-- answer never consumes it. Re-exported straight from 'Tidepool.Effects' (in the
-- import above) to match the @examples\/harness\/Harness.hs@ target contract's
-- @import Tidepool.Harness (Harness, runLLMTurn)@ — see the import haddock for
-- why it is a re-export and not a wrapper.
