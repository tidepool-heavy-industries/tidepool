-- | Typed reads over the 'KV' effect. The base 'kvGet' verb (declared in
-- 'Tidepool.Effects') is untyped JSON in and out
-- (@Text -> Eff effs (Either KvError (Maybe Value))@) — a caller storing a
-- typed shape otherwise has to hand-unwrap the 'Value' constructor on every read.
-- 'kvGetAs' decodes via 'FromJSON'
-- instead, same built-on-the-raw-substrate-verb shape as
-- 'Tidepool.Random.randomRIO' over 'entropySeed'.
module Tidepool.Kv
  ( kvGetAs
  ) where

import Prelude
import Data.Text (Text)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Effects.Authored (KV, KvError(..), kvGet)
import Tidepool.Aeson (FromJSON, fromJSON, resultToEither)

-- | Typed KV read: look up a key and decode the stored 'Value' via its
-- 'FromJSON' instance. 'Right Nothing' when the key is absent (mirrors
-- 'kvGet'\'s absence convention); storage failures pass through unchanged and
-- a decode mismatch becomes 'KvDecode'. These failures are returned as data,
-- not a crash. Reach for this when you stored a typed value and want it back
-- typed instead of pattern-matching 'Value' constructors by hand.
--
-- > r <- kvGetAs @Int "counter"   -- Either KvError (Maybe Int)
kvGetAs :: forall a effs.
  (FromJSON a, Member KV effs) => Text -> Eff effs (Either KvError (Maybe a))
kvGetAs k = do
  stored <- kvGet k
  pure $ case stored of
    Left err -> Left err
    Right Nothing -> Right Nothing
    Right (Just v) -> case resultToEither (fromJSON v) of
      Left detail -> Left (KvDecode detail)
      Right decoded -> Right (Just decoded)
