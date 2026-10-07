{-# LANGUAGE OverloadedStrings #-}

-- | The @Schema@ vocabulary behind @ask@ (`Tidepool.Effects.Core`'s @Ask@): a
-- small JSON-Schema-shaped sum, the pure recursion that renders it as a
-- 'Value', and @ask@ itself. @ask@ asks the calling agent for a structured
-- answer. This is separate from 'Tidepool.Form', which describes a human
-- operator form with typed controls and retains its submitted answer in the
-- dialogue.
--
-- @Ask@ is always present in the ordinary eval/session roster, so this module
-- is auto-imported wherever @Ask@ is available. The human form API belongs to
-- the separately gated @AskUser@ effect. Keeping this schema helper in its own
-- module lets ordinary structured questions remain available when @AskUser@
-- or @Llm@ is absent; @Tidepool.Llm@ shares the 'Schema' vocabulary but owns
-- its @llm@ operation separately.
--
-- @ask@ builds on the generated module's thin @askRaw@ wrapper. The generated
-- @Tidepool.Effects.Core@ module cannot import authored library code, so JSON
-- Schema rendering lives here and the underlying effect constructor stays in
-- the generated layer.
module Tidepool.Form.Schema
  ( Schema (..)
  , schemaToValue
  , isOpt
  , innerSchema
  , ask
  ) where

import Prelude
import Data.Text (Text)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Aeson (Value, object, (.=))
import Tidepool.Effects.Core (Ask, askRaw)

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
-- Carries @schema@ as JSON Schema in the suspension for the caller to use.
-- It describes the expected value; this module does not validate resume
-- replies. Extract fields from the returned 'Value' with optics, e.g.
-- @v ^? key "path" . _String@.
ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value
ask schema prompt = askRaw prompt (object ["schema" .= schemaToValue schema])
