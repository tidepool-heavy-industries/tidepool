{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The Shoal host transport for the Jev authoring surface: one Jev
-- request/response exchanged as JSON text with the host through the generated
-- @Jev@ effect (@Tidepool.Effects.Core@, constructor
-- @JevAskWith :: Text -> Jev (Either JevCallError Text)@ — request and
-- response bodies cross the effect boundary as JSON TEXT, not 'Value';
-- 'jevTransport' does the encode/decode at the boundary).
--
-- 'jevTransport' is NOT re-exported from the authoring surface: the documented
-- path for actor code is its @ask@ \/ @ask1@ \/ @askWith@, which already bind
-- a session over this transport. It is exported here only so tests can drive
-- the transport directly.
module Jev.Host
  ( jevTransport
  , renderJevCallError
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Aeson.Value (Value, eitherDecodeValue, encodeValue)
import Tidepool.Effects.Core (Jev (..), JevCallError (..))

-- | Encode a Jev request 'Value' to JSON text, send it through the @Jev@
-- effect, and decode the response body back to 'Value' — matching the
-- @Value -> m (Either Text Value)@ shape @Jev.Core.session@ binds into the
-- session that @Jev.Core.roundTrip@ and @Jev.Core.jev1@ take.
jevTransport :: Member Jev effs => Value -> Eff effs (Either Text Value)
jevTransport body = do
  reply <- send (JevAskWith (encodeValue body))
  pure $ case reply of
    Left failure -> Left (renderJevCallError failure)
    Right text -> eitherDecodeValue text

-- | Render a 'JevCallError' for the caller of 'jevTransport' — 'roundTrip'
-- and 'jev1' transports report failure as 'Left' 'Text', so this is where
-- the host's structured error collapses to the DSL's error channel.
renderJevCallError :: JevCallError -> Text
renderJevCallError = \case
  JevUnconfigured -> "jev: no Jev endpoint is configured for this run"
  JevCallCap -> "jev: call cap reached for this turn"
  JevTransport msg -> "jev: transport error: " <> msg
  JevTimeout -> "jev: request timed out"
  JevHttp code msg -> "jev: http " <> T.pack (show code) <> ": " <> msg
  JevBodyLimit -> "jev: response body exceeded the size limit"
  JevMalformed msg -> "jev: malformed response: " <> msg
