{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}
-- | Typed source publication through an admitted synchronous invocation.
module Tidepool.Reload
  ( ReloadInput (..), ReloadError (..), ReloadOutcome (..)
  , ReloadOptions (..), ReloadPolicy (..), ReloadSurfaceDiff (..)
  , ReloadRefusal (..), ReloadRefusalCategory (..)
  , reloadSource, reloadSourceWith, reloadHandler, reloadPresenter
  ) where
import Control.Monad.Freer (Eff, Member)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import GHC.Generics (Generic)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson.FromJSON (FromJSON (..), withObject, (.:?), (.!=))
import Tidepool.Aeson.Value (Value (..), ToJSON (..), object, (.=))
import Tidepool.Aeson.Schema (JsonSchema (..), objectSchema)
import Tidepool.Agent.ToolEffects (SyncEffects)
import Tidepool.Effects.Core (Reload, ReloadError (..), ReloadOutcome (..), ReloadOptions (..), ReloadPolicy (..), ReloadSurfaceDiff (..), ReloadRefusal (..), ReloadRefusalCategory (..), SourceRevision (..), reloadSourceWith)

-- | Optional hosted reload arguments. Omitted values preserve the tool surface.
data ReloadInput = ReloadInput
  { also_check :: [Text]
  , accept_cache_invalidation :: Bool
  } deriving (Show, Eq, Generic)

instance FromJSON ReloadInput where
  parseJSON = withObject "ReloadInput" $ \o -> ReloadInput
    <$> ((o .:? "also_check") .!= [])
    <*> ((o .:? "accept_cache_invalidation") .!= False)
instance ToJSON ReloadInput
instance JsonSchema ReloadInput where
  jsonSchema _ = object
    [ "type" .= ("object" :: Text)
    , "properties" .= object ["also_check" .= jsonSchema (Proxy :: Proxy [Text]), "accept_cache_invalidation" .= jsonSchema (Proxy :: Proxy Bool)]
    , "additionalProperties" .= False
    ]

instance JsonSchema ReloadError where
  jsonSchema _ = object [("oneOf", Array
    [ objectSchema Nothing [("tag", object ["enum" .= (["ReloadUnavailable"] :: [Text])], True), ("detail", jsonSchema (Proxy :: Proxy Text), True)]
    , objectSchema Nothing [("tag", object ["enum" .= (["ReloadUnreadable"] :: [Text])], True), ("detail", jsonSchema (Proxy :: Proxy Text), True)]
    ])]

-- | Compatible publication. Changed source may still change the prompt prefix.
reloadSource :: Member Reload effects => [Text] -> Eff effects (Either ReloadError ReloadOutcome)
reloadSource checks = reloadSourceWith (ReloadOptions checks CompatibleOnly)

reloadHandler :: ReloadInput -> Eff (SyncEffects effects) (Either ReloadError ReloadOutcome)
reloadHandler (ReloadInput checks accept) = reloadSourceWith
  (ReloadOptions checks (if accept then AcceptCacheInvalidation else CompatibleOnly))

reloadPresenter :: Either ReloadError ReloadOutcome -> Text
reloadPresenter result = case result of
  Left (ReloadUnavailable detail) -> "Reload unavailable: " <> detail
  Left (ReloadUnreadable detail) -> "Reload unreadable: " <> detail
  Right outcome -> case outcome of
    ReloadUnchanged _ -> "Configuration unchanged; notebook bindings preserved."
    ReloadPublished _ (SourceRevision identity _ _) _ diff -> "Published source " <> identity <> surfaceSummary diff
    ReloadRejected _ _ diagnostics -> "Preparation rejected: " <> diagnostics
    ReloadRefused _ _ (ReloadRefusal category detail diff) -> "Reload refused (" <> T.pack (show category) <> "): " <> detail <> surfaceSummary diff
    ReloadCancelled _ -> "Reload cancelled before publication."
    ReloadPublishedWithWarnings _ (SourceRevision identity _ _) _ warnings diff ->
      "Published source " <> identity <> surfaceSummary diff <> " Warnings: " <> T.pack (show warnings)
    ReloadPublicationUnconfirmed _ detail -> "Publication unconfirmed; reconcile before retry: " <> detail
  where
    surfaceSummary (ReloadSurfaceDiff added removed changed) =
      " (tools added " <> count added <> ", removed " <> count removed <> ", changed " <> count changed <> ")."
    count = T.pack . show . length

