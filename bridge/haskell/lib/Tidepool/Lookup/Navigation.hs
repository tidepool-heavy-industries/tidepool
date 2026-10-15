{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DeriveAnyClass #-}
-- | Pure navigation of one captured batch. Addresses never trigger lookup or IO.
module Tidepool.Lookup.Navigation
  ( LookupSection(..), LookupAddress(..), LookupLines(..), LookupSelection(..)
  , LookupHit(..), LookupExcerpt(..), LookupExtent(..), LookupSelectionIssue(..)
  , lookupIndex, renderLookupIndex, selectLookup, selectLookupMany, lookupSectionText
  ) where

import Prelude
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Aeson.Value (ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Lookup
import Tidepool.Text.Navigation (retainedLines)
import Tidepool.Inspection.Display
import Tidepool.Inspection.Tree

-- | Setup ordinals follow manifest order. Runtime requirements are descriptive,
-- not source files and not evidence that the example has been compiled.
data LookupSection = Signature | Contract | Example | Requirements | Setup Int | Manifest | Bundle | Source
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupAddress = LookupAddress { resultOrdinal :: Int, entryOrdinal :: Int, section :: LookupSection }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupLines = LookupLines { firstLine :: Int, lastLine :: Int } deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupSelection = LookupSelection { address :: LookupAddress, lineWindow :: Maybe LookupLines } deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupHit = LookupHit
  { hitResult :: Int, hitEntry :: Maybe Int, hitQuery :: Text, hitName :: Maybe Text
  , hitModule :: Maybe Text, hitStatus :: Text, hitSections :: [LookupSection]
  } deriving (Eq, Show)
data LookupSelectionIssue = InvalidResult | InvalidEntry | InvalidLines | LinesOutsideSource
  | ResultMissing [Text] [Text] | ResultRejected Text | SectionUnavailable Text
  deriving (Eq, Show)
-- Complete refers only to the selected captured section, never upstream completeness.
data LookupExtent = Complete | Partial [LookupSelection] deriving (Eq, Show)
data LookupExcerpt = LookupExcerpt
  { excerptBatchIdentity :: Text, excerptView :: Text, excerptAddress :: LookupAddress, excerptQuery :: Text
  , excerptName :: Text, excerptModule :: Maybe Text, excerptOrigin :: LookupOrigin
  , excerptAvailability :: LookupAvailability, excerptQuality :: LookupQuality
  , excerptLocator :: Maybe Text, excerptSourceIdentity :: Maybe Text
  , excerptLines :: Maybe LookupLines, excerptText :: Text, excerptExtent :: LookupExtent
  , excerptUnavailable :: [Text]
  } deriving (Eq, Show)

lookupIndex :: LookupBatch -> [LookupHit]
lookupIndex batch = concatMap index (zip [1..] (lookupResults batch))
  where
    index (r, result) = case lookupOutcome result of
      LookupFound entries omitted -> entriesAt r result (if omitted then "found; additional matches omitted" else "found") entries
      LookupAmbiguous entries omitted -> entriesAt r result (if omitted then "ambiguous; additional matches omitted" else "ambiguous") entries
      LookupMissing _ _ -> [empty r result "missing"]
      LookupRejected _ -> [empty r result "rejected; diagnostic retained"]
    empty r result status = LookupHit r Nothing (lookupQuery result) Nothing Nothing status []
    entriesAt r result status [] = [empty r result (status <> "; no entries retained")]
    entriesAt r result status entries =
      [LookupHit r (Just e) (lookupQuery result) (Just (lookupName entry)) (lookupModule entry) status (sections entry)
      | (e, entry) <- zip [1..] entries]
    sections entry = [Signature] ++ [Contract | lookupContract entry /= Nothing]
      ++ case lookupExample entry of
        Nothing -> []
        Just example -> [Example, Requirements, Manifest, Bundle] ++ map Setup [1..length (exampleSetup example)]

-- | Compact hit index; detailed source stays in the captured fields.
renderLookupIndex :: LookupBatch -> Text
renderLookupIndex batch = "Capture " <> lookupIdentity batch <> " (view " <> lookupView batch <> ")\n"
  <> T.intercalate "\n" [T.pack (show (hitResult hit)) <> maybe "" (("." <>) . T.pack . show) (hitEntry hit)
     <> " " <> hitQuery hit <> maybe "" (" — " <>) (hitName hit)
     <> maybe "" (\owner -> " (" <> owner <> ")") (hitModule hit)
     <> " [" <> hitStatus hit <> "] " <> T.pack (show (hitSections hit)) | hit <- lookupIndex batch]
  <> maybe "" ("\nLookup issue: " <>) (lookupIssue batch)

selectLookupMany :: LookupBatch -> [LookupSelection] -> [Either LookupSelectionIssue LookupExcerpt]
selectLookupMany batch = map (selectLookup batch)

selectLookup :: LookupBatch -> LookupSelection -> Either LookupSelectionIssue LookupExcerpt
selectLookup batch selection = do
  let at = address selection
  result <- ordinal InvalidResult (resultOrdinal at) (lookupResults batch)
  entries <- case lookupOutcome result of
    LookupFound values _ -> Right values
    LookupAmbiguous values _ -> Right values
    LookupMissing attempted suggestions -> Left (ResultMissing attempted suggestions)
    LookupRejected issue -> Left (ResultRejected issue)
  entry <- ordinal InvalidEntry (entryOrdinal at) entries
  (locator, identity, source) <- lookupSectionText (section at) entry
  (rows, text, extent) <- window selection source
  pure (LookupExcerpt (lookupIdentity batch) (lookupView batch) at (lookupQuery result) (lookupName entry)
    (lookupModule entry) (lookupOrigin entry) (lookupAvailability entry) (lookupQuality entry) locator identity rows text extent (unavailable (section at) entry))

unavailable :: LookupSection -> LookupEntry -> [Text]
unavailable Bundle entry = case lookupExample entry of
  Nothing -> []
  Just example -> maybe [] (\issue -> [exampleLocator example <> ": " <> issue]) (exampleSourceIssue example) ++ [lookupSourceLocator source <> ": " <> maybe "source not retained" id (lookupSourceIssue source)
                 | source <- exampleSetup example, lookupSourceText source == Nothing]
unavailable _ _ = []

ordinal :: issue -> Int -> [a] -> Either issue a
ordinal issue n values | n < 1 = Left issue
                       | otherwise = case drop (n - 1) values of
                           value:_ -> Right value
                           [] -> Left issue

-- | Exact captured section text, plus its owner-issued identity when available.
-- A signature is not the full declaration source. Never synthesize Source from it.
lookupSectionText :: LookupSection -> LookupEntry -> Either LookupSelectionIssue (Maybe Text, Maybe Text, Text)
lookupSectionText Signature entry = Right (Nothing, Nothing, lookupDeclaration entry)
lookupSectionText Contract entry = case lookupContract entry of
  Nothing -> Left (SectionUnavailable "no owner-authored contract captured")
  Just text -> Right (Nothing, Nothing, text)
lookupSectionText Source _ = Left (SectionUnavailable "defining source was not captured by its owner")
lookupSectionText selected entry = case lookupExample entry of
  Nothing -> Left (SectionUnavailable "no registered example captured")
  Just example -> case selected of
    Example -> case exampleSourceIssue example of
      Just issue -> Left (SectionUnavailable issue)
      Nothing -> Right (Just (exampleLocator example), exampleSourceIdentity example, exampleSource example)
    Requirements -> Right (Nothing, Nothing, exampleRequirements example)
    Setup n -> do
      source <- ordinal InvalidEntry n (exampleSetup example)
      case lookupSourceText source of
        Nothing -> Left (SectionUnavailable (maybe "setup source unavailable" id (lookupSourceIssue source)))
        Just text -> Right (Just (lookupSourceLocator source), lookupSourceIdentity source, text)
    Manifest -> Right (Nothing, Nothing, manifest example)
    Bundle -> Right (Nothing, Nothing, bundle example)
  where
    manifest example = "Main: " <> exampleLocator example <> maybe "" (" — unavailable: " <>) (exampleSourceIssue example) <> "\nRuntime requirements: " <> exampleRequirements example
      <> T.concat ["\nSetup " <> T.pack (show n) <> ": " <> lookupSourceLocator source
           <> maybe "" (" — unavailable: " <>) (lookupSourceIssue source)
         | (n, source) <- zip [1 :: Int ..] (exampleSetup example)]
    bundle example = manifest example <> "\n\n" <> exampleLocator example <> "\n" <> maybe (exampleSource example) ("Unavailable: " <>) (exampleSourceIssue example)
      <> T.concat ["\n\n" <> lookupSourceLocator source <> "\n" <> maybe ("Unavailable: " <> maybe "unknown" id (lookupSourceIssue source)) id (lookupSourceText source)
         | source <- exampleSetup example]


window :: LookupSelection -> Text -> Either LookupSelectionIssue (Maybe LookupLines, Text, LookupExtent)
window selection source = case lineWindow selection of
  Nothing -> Right (if null rows then Nothing else Just (LookupLines 1 count), source, Complete)
  Just (LookupLines first last')
    | first < 1 || last' < first -> Left InvalidLines
    | first > count || last' > count -> Left LinesOutsideSource
    | otherwise -> Right (Just (LookupLines first last'), T.concat (take (last' - first + 1) (drop (first - 1) rows)),
        if first == 1 && last' == count then Complete
        else Partial ([LookupSelection (address selection) (Just (LookupLines 1 (first - 1))) | first > 1]
          ++ [LookupSelection (address selection) (Just (LookupLines (last' + 1) count)) | last' < count]))
  where rows = retainedLines source
        count = length rows

instance Display LookupHit where
  displayTree hit = Constructor "LookupHit"
    [("result", displayTree (hitResult hit)), ("entry", displayTree (hitEntry hit)),
     ("query", displayTree (hitQuery hit)), ("name", displayTree (hitName hit)),
     ("module", displayTree (hitModule hit)), ("status", TextLeaf (hitStatus hit)),
     ("sections", TextLeaf (T.pack (show (hitSections hit))))]
instance Display LookupExcerpt where
  displayTree excerpt = Constructor "LookupExcerpt"
    [("batch", displayTree (excerptBatchIdentity excerpt)), ("view", displayTree (excerptView excerpt)), ("address", TextLeaf (T.pack (show (excerptAddress excerpt)))),
     ("query", displayTree (excerptQuery excerpt)), ("name", displayTree (excerptName excerpt)),
     ("module", displayTree (excerptModule excerpt)), ("origin", TextLeaf (T.pack (show (excerptOrigin excerpt)))),
     ("availability", TextLeaf (T.pack (show (excerptAvailability excerpt)))), ("quality", TextLeaf (T.pack (show (excerptQuality excerpt)))),
     ("locator", displayTree (excerptLocator excerpt)), ("sourceIdentity", displayTree (excerptSourceIdentity excerpt)),
     ("lines", TextLeaf (T.pack (show (excerptLines excerpt)))), ("extent", TextLeaf (T.pack (show (excerptExtent excerpt)))),
     ("unavailable", displayTree (excerptUnavailable excerpt)), ("text", TextLeaf (excerptText excerpt))]
instance Display LookupSelectionIssue where displayTree = TextLeaf . T.pack . show
