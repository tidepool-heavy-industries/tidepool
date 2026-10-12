{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
-- | State-rooted observations. Existing memory paths are preserved; the optional
-- manifest adds sources without turning a missing file into an empty memory.
module Persona.Collection
  ( Entry(..), Manifest(..), LoadError(..), Loaded(..)
  , manifestFromValue, load, loadEntries
  ) where

import Prelude hiding (readFile)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Aeson (FromJSON(..), Value, Result(..), withObject, (.:), (.:?), (.!=), eitherDecode, fromJSON, resultToEither)
import Tidepool.Effects.Authored (FsRead, FsWrite, readFile)
import Tidepool.Records (FsError(..), WriteOutcome)
import Tidepool.Inspection.Display (Display(..))
import Tidepool.Inspection.Tree (DisplayTree(..))
import qualified Persona.Memory as M
import qualified Persona.Attention as A

data Entry = Entry
  { entryId :: Text, entryPath :: Text, entryTags :: [Text]
  , entryRelated :: [Text], entryAvailableAt :: Maybe Integer
  } deriving (Eq, Show)
instance FromJSON Entry where
  parseJSON = withObject "memory entry" $ \o -> Entry
    <$> o .: "id" <*> o .: "path" <*> (o .:? "tags" .!= [])
    <*> (o .:? "related" .!= []) <*> o .:? "availableAt"
newtype Manifest = Manifest [Entry] deriving (Eq, Show)
instance FromJSON Manifest where
  parseJSON = withObject "memory collection manifest" $ \o -> do
    version <- o .: "version"
    if (version :: Int) /= 1 then Error "Expected memory manifest version 1"
    else Manifest <$> o .: "memories"
data LoadError = Storage Text FsError | ManifestInvalid Text | InvalidCollection A.Problem
  | InitialisationFailed [Either FsError WriteOutcome]
  deriving (Eq, Show)
instance Display LoadError where displayTree = displayTree . T.pack . show
data Loaded = Loaded
  { collection :: A.Collection, initialisation :: [Either FsError WriteOutcome]
  , manifestObservation :: Maybe Text
  } deriving (Eq, Show)
instance Display Loaded where
  displayTree loaded = Constructor "LoadedMemory"
    [("collection", displayTree (collection loaded)),
     ("initialisation", displayTree (T.pack (show (initialisation loaded)))),
     ("manifestObservation", displayTree (manifestObservation loaded))]

manifestFromValue :: Value -> Either LoadError Manifest
manifestFromValue = either (Left . ManifestInvalid) Right . resultToEither . fromJSON

load :: (Member FsRead effects, Member FsWrite effects) => Eff effects (Either LoadError Loaded)
load = do
  outcomes <- M.initialise
  case sequence outcomes of
    Left _ -> pure (Left (InitialisationFailed outcomes))
    Right _ -> do
      observed <- readFile "memory/collection.json"
      case observed of
        Left (FsNotFound _) -> assemble outcomes Nothing (Manifest [])
        Left err -> pure (Left (Storage "memory/collection.json" err))
        Right body -> case eitherDecode body of
          Left err -> pure (Left (ManifestInvalid err))
          Right manifest -> assemble outcomes (Just body) manifest
  where
    assemble outcomes observed manifest = do
      result <- loadEntries manifest
      pure ((\values -> Loaded values outcomes observed) <$> result)

-- | Public composition for an already parsed or Haskell-authored manifest.
-- Validate identities before reading extra sources. FsRead owns path authority.
loadEntries :: Member FsRead effects => Manifest -> Eff effects (Either LoadError A.Collection)
loadEntries (Manifest extra) = case A.collection (map (memory "") entries) of
  Left problem -> pure (Left (InvalidCollection problem))
  Right _ | any (T.null . T.strip . entryId) entries -> pure (Left (ManifestInvalid "Empty memory ID"))
  Right _ -> do
    results <- traverse observe entries
    pure $ sequence results >>= either (Left . InvalidCollection) Right . A.collection
  where
    entries = [Entry "persona" "memory/persona.md" [] [] Nothing,
               Entry "human" "memory/human.md" [] [] Nothing,
               Entry "curiosities" "memory/curiosities.md" [] [] Nothing] <> extra
    memory body e = A.Memory (entryId e) (entryPath e) body (entryTags e) (entryRelated e) (entryAvailableAt e)
    observe e = do
      value <- readFile (entryPath e)
      pure $ either (Left . Storage (entryPath e)) (Right . (`memory` e)) value
