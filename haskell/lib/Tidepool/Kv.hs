-- | Typed reads over the 'KV' effect. The base 'kvGet' verb (declared in
-- 'Tidepool.Effects') is untyped-JSON-in, untyped-JSON-out (@Text -> Eff
-- effs (Maybe Value)@) — a caller storing a typed shape has to hand-unwrap
-- the 'Value' constructor on every read. 'kvGetAs' decodes via 'FromJSON'
-- instead, same built-on-the-raw-substrate-verb shape as
-- 'Tidepool.Random.randomRIO' over 'entropySeed'.
module Tidepool.Kv
  ( kvGetAs
  ) where

import Prelude
import Data.Text (Text)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Effects (KV, kvGet)
import Tidepool.Aeson (FromJSON, fromJSON, resultToEither)

-- | Typed KV read: look up a key and decode the stored 'Value' via its
-- 'FromJSON' instance. 'Right Nothing' when the key is absent (mirrors
-- 'kvGet'\'s own absence convention); 'Left' with a decode-failure reason
-- when the key is present but the stored JSON doesn't match @a@\'s shape —
-- legible data, not a crash. The untyped 'kvGet' is unchanged; reach for
-- this when you stored a typed value and want it back typed instead of
-- pattern-matching 'Value' constructors by hand.
--
-- > r <- kvGetAs @Int "counter"   -- Either Text (Maybe Int)
kvGetAs :: forall a effs. (FromJSON a, Member KV effs) => Text -> Eff effs (Either Text (Maybe a))
kvGetAs k = do
  mv <- kvGet k
  pure $ case mv of
    Nothing -> Right Nothing
    Just v  -> Just <$> resultToEither (fromJSON v)
