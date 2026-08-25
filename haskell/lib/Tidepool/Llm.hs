{-# LANGUAGE OverloadedStrings #-}

-- | The composed @llm@ verb: builds on the generated module's thin @llmRaw@
-- (`Tidepool.Effects.Core`'s @Llm@) the same way `Tidepool.Form.Schema`'s
-- @ask@ builds on @askRaw@ — the generated @Tidepool.Effects.Core@ module
-- cannot import authored library code, so only the bare @send (Ctor …)@
-- wrapper stays there, and anything composed on top (here, JSON-Schema
-- rendering, shared with @ask@) lives here instead.
--
-- Deliberately a SEPARATE module from `Tidepool.Form.Schema`, even though
-- @llm@ reuses that module's 'Schema' vocabulary: @Llm@ and @Ask@ are two
-- INDEPENDENTLY-gated effects — @Ask@ is universal (@EffectRoster::
-- from_handlers@ unconditionally appends it to every stack,
-- @tidepool-mcp/src/server.rs@) but @Llm@ is only present in stacks that
-- actually wire an @LlmHandler@ (@build_base_stack@; NOT
-- @build_minimal_stack@'s Console-only rosters). Folding @llm@ into
-- `Tidepool.Form.Schema` (auto-imported unconditionally whenever @Ask@ is
-- present, via @extra_imports_for!(Ask)@ in
-- @tidepool-mcp/src/effect_defs.rs@) forced every Ask-only roster to also
-- resolve @Llm@'s GADT just to typecheck that one file — a regression an
-- Ask-without-Llm roster (e.g. any @tidepool-repl@ decl-plane test) tripped
-- over. This module gets its OWN @extra_imports_for!(Llm)@ arm instead, so it
-- is only ever compiled on a roster that actually has @Llm@.
module Tidepool.Llm
  ( llm
  ) where

import Prelude
import Data.Text (Text)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Aeson (Value)
import Tidepool.Effects.Core (Llm, llmRaw)
import Tidepool.Form.Schema (Schema, schemaToValue)
import Tidepool.Records.Stable (LlmError)

-- | Call an LLM for structured output. Failure is TYPED and TOTAL (#335):
-- @Left (LlmApi _)@ on an API/network failure, @Left (LlmRefusal _)@ on a
-- declined answer, @Left LlmBudget@ when the per-eval call budget is
-- exhausted — none of these abort the eval. Unwrap with @Right v <- llm
-- schema prompt@ or @>>= liftEither@.
llm :: forall effs. Member Llm effs => Schema -> Text -> Eff effs (Either LlmError Value)
llm schema prompt = llmRaw prompt (schemaToValue schema)
