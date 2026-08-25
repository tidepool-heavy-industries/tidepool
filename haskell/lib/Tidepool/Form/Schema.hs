{-# LANGUAGE OverloadedStrings #-}

-- | The @Schema@ vocabulary shared by @ask@ and @llm@ (`Tidepool.Effects`'
-- @Ask@/@Llm@): a small JSON-Schema-shaped sum, the pure recursion that
-- renders it as an actual JSON Schema 'Value', and @ask@\/@llm@ themselves.
--
-- Deliberately NOT under 'Tidepool.Form' proper (which builds on
-- @askUserRaw@ and only compiles in a row containing @AskUser@): @Ask@ is
-- always present in the ordinary eval\/session roster (unlike the gated
-- @AskUser@), so this module is auto-imported unconditionally whenever
-- @Ask@ is (see @extra_imports_for!@ in @tidepool-mcp/src/effect_defs.rs@),
-- independent of whether @AskUser@'s @Tidepool.Form@ is in the row at all.
--
-- @ask@\/@llm@ build on the generated module's thin @askRaw@\/@llmRaw@ the
-- same way 'Tidepool.Form'\'s @askUser@ builds on @askUserRaw@: the generated
-- @Tidepool.Effects@ module cannot import authored library code, so only the
-- bare @send (Ctor …)@ wrappers stay there, and anything composed on top —
-- here, JSON-Schema rendering — lives here instead.
module Tidepool.Form.Schema
  ( Schema (..)
  , schemaToValue
  , isOpt
  , innerSchema
  , ask
  , llm
  ) where

import Prelude
import Data.Text (Text)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Aeson (Value, object, (.=))
import Tidepool.Effects (Ask, Llm, askRaw, llmRaw)
import Tidepool.Records.Stable (LlmError)

-- | @ask@\/@llm@'s shared request shape: a JSON-Schema-shaped sum, not a raw
-- JSON 'Value' — see 'schemaToValue' for the rendering.
data Schema
  = SObj [(Text, Schema)]
  | SArr Schema
  | SStr
  | SNum
  | SBool
  | SEnum [Text]
  | SOpt Schema

isOpt :: Schema -> Bool
isOpt (SOpt _) = True
isOpt _ = False

innerSchema :: Schema -> Schema
innerSchema (SOpt s) = s
innerSchema s = s

-- | Render a 'Schema' as an actual JSON Schema object. An 'SOpt' field is
-- unwrapped to its inner schema and dropped from the object's @required@
-- list, rather than rendered as its own JSON Schema shape.
schemaToValue :: Schema -> Value
schemaToValue SStr = object ["type" .= ("string" :: Text)]
schemaToValue SNum = object ["type" .= ("number" :: Text)]
schemaToValue SBool = object ["type" .= ("boolean" :: Text)]
schemaToValue (SEnum vs) = object ["type" .= ("string" :: Text), "enum" .= vs]
schemaToValue (SArr item) = object ["type" .= ("array" :: Text), "items" .= schemaToValue item]
schemaToValue (SOpt s) = schemaToValue s
schemaToValue (SObj fields) = object ["type" .= ("object" :: Text), "properties" .= object (map (\(k,s) -> k .= schemaToValue (innerSchema s)) fields), "required" .= map fst (filter (not . isOpt . snd) fields)]

-- | Suspend execution and ask the calling agent a STRUCTURED question.
-- Carries @schema@ as JSON Schema in the suspension; the resume reply is
-- validated against it server-side before re-entering the computation
-- (invalid replies do NOT consume the continuation). Extract fields from the
-- returned 'Value' with optics, e.g. @v ^? key "path" . _String@.
ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value
ask schema prompt = askRaw prompt (object ["schema" .= schemaToValue schema])

-- | Call an LLM for structured output. Failure is TYPED and TOTAL (#335):
-- @Left (LlmApi _)@ on an API/network failure, @Left (LlmRefusal _)@ on a
-- declined answer, @Left LlmBudget@ when the per-eval call budget is
-- exhausted — none of these abort the eval. Unwrap with @Right v <- llm
-- schema prompt@ or @>>= liftEither@.
llm :: forall effs. Member Llm effs => Schema -> Text -> Eff effs (Either LlmError Value)
llm schema prompt = llmRaw prompt (schemaToValue schema)
