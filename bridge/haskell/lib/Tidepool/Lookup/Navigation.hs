{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DeriveAnyClass #-}
-- | Pure navigation of one captured batch. Addresses never trigger lookup or IO.
module Tidepool.Lookup.Navigation
  ( LookupSection(..), LookupAddress(..), LookupLines(..), LookupSelection(..)
  , LookupHit(..), LookupExcerpt(..), LookupExtent(..), LookupSelectionIssue(..)
  , lookupIndex, lookupIndexTree, renderLookupIndex, selectLookup, selectLookupMany, lookupSectionText
  , RetainedLookupCapture, LookupCapturePart(..), LookupCaptureSelection(..)
  , LookupCaptureExcerpt(..), LookupCaptureIssue(..), retainLookupCapture
  , lookupCaptureIndex, lookupCaptureIndexTree, renderLookupCaptureIndex, lookupCaptureReceipts
  , selectLookupCapture, selectLookupCaptureMany
  , LookupLiteralQuery(..), LookupLiteralSearch(..), LookupLiteralMatch(..)
  , LookupCaptureTextSelection(..), LookupCaptureText(..)
  , searchLookupCapture, selectLookupCaptureText, selectLookupCaptureTextMany
  ) where

import Prelude
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Aeson.Value (ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Lookup
import Tidepool.Text.Navigation (retainedLines, literalSearch, LiteralMatches(..), TextSlice(..), TextSelectionIssue)
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

-- | The existing two-stage authored lookup policy. Each native batch and its
-- receipt remain unchanged; no combined native search outcome is invented.
data RetainedLookupCapture = RetainedLookupCapture LookupBatch (Maybe LookupBatch)
data LookupCapturePart = PrimaryLookup | RelatedLookup
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupCaptureSelection = LookupCaptureSelection
  { selectedCapture :: LookupCapturePart, capturedSelection :: LookupSelection }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupCaptureExcerpt = LookupCaptureExcerpt
  { captureExcerptPart :: LookupCapturePart, captureExcerpt :: LookupExcerpt
  , captureContinuations :: [LookupCaptureSelection] }
  deriving (Eq, Show)
data LookupCaptureIssue = CaptureNotRetained LookupCapturePart
  | CaptureRefused LookupCapturePart LookupSearchReceipt (Maybe Text)
  | CaptureViewMismatch Text Text | CaptureBatchMismatch Text Text
  | CaptureSelectionIssue LookupSelectionIssue
  | CaptureTextIssue TextSelectionIssue | CaptureInvalidTextRange Int Int Int
  | CaptureInvalidSearchBounds Int Int
  deriving (Eq, Show)

-- | Offsets are 0-based half-open Unicode scalar ranges in one complete
-- retained section, independent of its line lengths or display truncation.
-- The issuing batch and view travel with every reusable location.
data LookupCaptureTextSelection = LookupCaptureTextSelection
  { textCapture :: LookupCapturePart, textAddress :: LookupAddress
  , textBatchIdentity :: Text, textView :: Text
  , textStart :: Int, textEnd :: Int }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupCaptureText = LookupCaptureText
  { captureTextSelection :: LookupCaptureTextSelection
  , captureTextBatchIdentity :: Text, captureTextView :: Text
  , captureTextLocator :: Maybe Text, captureTextSourceIdentity :: Maybe Text
  , captureTextUnavailable :: [Text], captureTextSlice :: TextSlice
  } deriving (Eq, Show)
-- | Caller-selected presentation budgets. Zero matches requests the count only;
-- negative limits or radii are refused. No upstream lookup policy is involved.
data LookupLiteralQuery = LookupLiteralQuery
  { literalCapture :: LookupCapturePart, literalAddress :: LookupAddress
  , literalNeedle :: Text, literalRadius :: Int, literalLimit :: Int }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)
data LookupLiteralMatch = LookupLiteralMatch
  { literalMatchSelection :: LookupCaptureTextSelection
  , literalContextSelection :: LookupCaptureTextSelection
  , literalMatchContext :: TextSlice }
  deriving (Eq, Show)
data LookupLiteralSearch = LookupLiteralSearch
  { literalSource :: LookupCaptureTextSelection
  , literalBatchIdentity :: Text, literalView :: Text
  , literalLocator :: Maybe Text, literalSourceIdentity :: Maybe Text
  , literalUnavailable :: [Text], literalCount :: Int
  , literalMatches :: [LookupLiteralMatch], literalOmitted :: Int }
  deriving (Eq, Show)

-- | Search only the named captured section. Counts cover all overlapping
-- occurrences; only the requested prefix receives contexts and reusable ranges.
searchLookupCapture :: RetainedLookupCapture -> LookupLiteralQuery -> Either LookupCaptureIssue LookupLiteralSearch
searchLookupCapture captured query = do
  selected <- selectLookupCapture captured (LookupCaptureSelection part (LookupSelection at Nothing))
  if literalRadius query < 0 || literalLimit query < 0
    then Left (CaptureInvalidSearchBounds (literalRadius query) (literalLimit query))
    else pure ()
  let excerpt = captureExcerpt selected
      source = excerptText excerpt
      location start end = LookupCaptureTextSelection part at
        (excerptBatchIdentity excerpt) (excerptView excerpt) start end
  matches <- either (Left . CaptureTextIssue) Right
    (literalSearch (excerptBatchIdentity excerpt) (literalRadius query) (literalNeedle query) source)
  -- take 0 never demands the zipped context list; counting forces only the
  -- literal search locations, never context slices or their text bodies.
  let bounded = take (literalLimit query) (zip (occurrenceLocations matches) (occurrenceContexts matches))
      hits = [LookupLiteralMatch (location start end) (location (sliceStart context) (sliceEnd context)) context
             | ((start, end), context) <- bounded]
  pure (LookupLiteralSearch (location 0 (T.length source))
    (excerptBatchIdentity excerpt) (excerptView excerpt) (excerptLocator excerpt) (excerptSourceIdentity excerpt)
    (excerptUnavailable excerpt) (occurrenceCount matches) hits (occurrenceCount matches - length hits))
  where part = literalCapture query
        at = literalAddress query

selectLookupCaptureText :: RetainedLookupCapture -> LookupCaptureTextSelection -> Either LookupCaptureIssue LookupCaptureText
selectLookupCaptureText captured selection = do
  batch <- captureBatch captured (textCapture selection)
  if textBatchIdentity selection /= lookupIdentity batch
    then Left (CaptureBatchMismatch (textBatchIdentity selection) (lookupIdentity batch))
    else pure ()
  if textView selection /= lookupView batch
    then Left (CaptureViewMismatch (textView selection) (lookupView batch))
    else pure ()
  excerpt <- either (Left . CaptureSelectionIssue) Right
    (selectLookup batch (LookupSelection (textAddress selection) Nothing))
  let source = excerptText excerpt
      size = T.length source
      start = textStart selection
      end = textEnd selection
  if start < 0 || end < start || end > size
    then Left (CaptureInvalidTextRange start end size)
    else pure (LookupCaptureText selection (excerptBatchIdentity excerpt) (excerptView excerpt)
      (excerptLocator excerpt) (excerptSourceIdentity excerpt) (excerptUnavailable excerpt)
      (TextSlice (excerptBatchIdentity excerpt) start end (T.take (end - start) (T.drop start source))
        (start > 0) (end < size)))

selectLookupCaptureTextMany :: RetainedLookupCapture -> [LookupCaptureTextSelection] -> [Either LookupCaptureIssue LookupCaptureText]
selectLookupCaptureTextMany captured = map (selectLookupCaptureText captured)

retainLookupCapture :: LookupBatch -> Maybe LookupBatch -> RetainedLookupCapture
retainLookupCapture = RetainedLookupCapture

lookupCaptureReceipts :: RetainedLookupCapture -> [(LookupCapturePart, LookupSearchReceipt)]
lookupCaptureReceipts (RetainedLookupCapture primary related) =
  (PrimaryLookup, lookupSearchReceipt primary) : maybe [] (\batch -> [(RelatedLookup, lookupSearchReceipt batch)]) related

captureBatch :: RetainedLookupCapture -> LookupCapturePart -> Either LookupCaptureIssue LookupBatch
captureBatch (RetainedLookupCapture primary related) part = do
  batch <- case part of
    PrimaryLookup -> Right primary
    RelatedLookup -> maybe (Left (CaptureNotRetained RelatedLookup)) Right related
  if lookupView batch /= lookupView primary
    then Left (CaptureViewMismatch (lookupView primary) (lookupView batch))
    else case searchDisposition (lookupSearchReceipt batch) of
      LookupSearchCaptured -> Right batch
      _ -> Left (CaptureRefused part (lookupSearchReceipt batch) (lookupIssue batch))

lookupCaptureIndex :: RetainedLookupCapture -> [(LookupCapturePart, LookupHit)]
lookupCaptureIndex captured = concat
  [either (const []) (map ((,) part) . lookupIndex) (captureBatch captured part)
  | part <- [PrimaryLookup, RelatedLookup]]

renderLookupCaptureIndex :: RetainedLookupCapture -> Text
renderLookupCaptureIndex = completeIndexText . lookupCaptureIndexTree

lookupCaptureIndexTree :: RetainedLookupCapture -> DisplayTree
lookupCaptureIndexTree captured@(RetainedLookupCapture _ related) = indexLines
  [Concat [StringLeaf (show part), LineBreak, either issueTree lookupIndexTree (captureBatch captured part)]
  | part <- PrimaryLookup : maybe [] (const [RelatedLookup]) related]
  where issueTree (CaptureRefused _ _ issue) = Concat
          [TextLeaf "Unavailable; inspect LookupReceipt.", maybe (Concat []) (\text -> Concat [TextLeaf " ", TextLeaf text]) issue]
        issueTree issue = StringLeaf (show issue)

selectLookupCapture :: RetainedLookupCapture -> LookupCaptureSelection -> Either LookupCaptureIssue LookupCaptureExcerpt
selectLookupCapture captured (LookupCaptureSelection part selection) = do
  batch <- captureBatch captured part
  excerpt <- either (Left . CaptureSelectionIssue) Right (selectLookup batch selection)
  let continuations = case excerptExtent excerpt of
        Complete -> []
        Partial selections -> map (LookupCaptureSelection part) selections
  pure (LookupCaptureExcerpt part excerpt continuations)

selectLookupCaptureMany :: RetainedLookupCapture -> [LookupCaptureSelection] -> [Either LookupCaptureIssue LookupCaptureExcerpt]
selectLookupCaptureMany captured = map (selectLookupCapture captured)

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
        Just example -> [Example, Requirements, Manifest, Bundle] ++ zipWith (\n _ -> Setup n) [1..] (exampleSetup example)

-- | Compact hit index; detailed source stays in the captured fields.
renderLookupIndex :: LookupBatch -> Text
renderLookupIndex = completeIndexText . lookupIndexTree

-- | The display cursor consumes only the requested prefix. Metadata and source
-- remain in the original batch; this tree does not flatten later hit rows.
lookupIndexTree :: LookupBatch -> DisplayTree
lookupIndexTree batch = Concat
  [ TextLeaf "Capture ", TextLeaf (lookupIdentity batch), TextLeaf " (view ", TextLeaf (lookupView batch), TextLeaf ")\n"
  , indexLines (map hitTree (lookupIndex batch))
  , maybe (Concat []) (\issue -> Concat [TextLeaf "\nLookup issue: ", TextLeaf issue]) (lookupIssue batch)
  ]
  where
    hitTree hit = Concat
      [ StringLeaf (show (hitResult hit))
      , maybe (Concat []) (\ordinal -> Concat [TextLeaf ".", StringLeaf (show ordinal)]) (hitEntry hit)
      , TextLeaf " ", TextLeaf (hitQuery hit)
      , maybe (Concat []) (\name -> Concat [TextLeaf " — ", TextLeaf name]) (hitName hit)
      , maybe (Concat []) (\owner -> Concat [TextLeaf " (", TextLeaf owner, TextLeaf ")"]) (hitModule hit)
      , TextLeaf " [", TextLeaf (hitStatus hit), TextLeaf "] ", StringLeaf (show (hitSections hit))
      ]

indexLines :: [DisplayTree] -> DisplayTree
indexLines [] = Concat []
indexLines (row : rest) = Concat (row : concatMap (\next -> [LineBreak, next]) rest)

-- An explicit Text request consumes the same formatting through to completion.
completeIndexText :: DisplayTree -> Text
completeIndexText tree = let (text, _, _) = renderTree maxBound tree in text

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
     ("sections", StringLeaf (show (hitSections hit)))]
numberedExcerptTree :: LookupExcerpt -> DisplayTree
numberedExcerptTree excerpt = StringLeaf (rows [first..] (T.unpack (excerptText excerpt)))
  where first = case excerptLines excerpt of Just (LookupLines start _) -> start; Nothing -> 1
        rows _ [] = []
        rows [] _ = []
        rows (number : following) text =
          let (body, rest) = break (== '\n') text
          in show number ++ ": " ++ body ++ case rest of
            [] -> []
            newline : remaining -> newline : rows following remaining

instance Display LookupExcerpt where
  displayTree excerpt = Constructor "LookupExcerpt"
    [("batch", displayTree (excerptBatchIdentity excerpt)), ("view", displayTree (excerptView excerpt)), ("address", TextLeaf (T.pack (show (excerptAddress excerpt)))),
     ("query", displayTree (excerptQuery excerpt)), ("name", displayTree (excerptName excerpt)),
     ("module", displayTree (excerptModule excerpt)), ("origin", TextLeaf (T.pack (show (excerptOrigin excerpt)))),
     ("availability", TextLeaf (T.pack (show (excerptAvailability excerpt)))), ("quality", TextLeaf (T.pack (show (excerptQuality excerpt)))),
     ("locator", displayTree (excerptLocator excerpt)), ("sourceIdentity", displayTree (excerptSourceIdentity excerpt)),
     ("lines", TextLeaf (T.pack (show (excerptLines excerpt)))), ("extent", TextLeaf (T.pack (show (excerptExtent excerpt)))),
     ("unavailable", displayTree (excerptUnavailable excerpt)), ("text", numberedExcerptTree excerpt)]
instance Display LookupSelectionIssue where displayTree = TextLeaf . T.pack . show
instance Display LookupCaptureExcerpt where
  displayTree excerpt = Constructor "LookupCaptureExcerpt"
    [("part", TextLeaf (T.pack (show (captureExcerptPart excerpt)))),
     ("excerpt", displayTree (captureExcerpt excerpt)),
     ("continuations", TextLeaf (T.pack (show (captureContinuations excerpt))))]
instance Display LookupCaptureIssue where displayTree = TextLeaf . T.pack . show

instance Display LookupCaptureTextSelection where displayTree = TextLeaf . T.pack . show
instance Display LookupCaptureText where
  displayTree value = Constructor "LookupCaptureText"
    [("selection", displayTree (captureTextSelection value)),
     ("batch", displayTree (captureTextBatchIdentity value)), ("view", displayTree (captureTextView value)),
     ("locator", displayTree (captureTextLocator value)), ("sourceIdentity", displayTree (captureTextSourceIdentity value)),
     ("unavailable", displayTree (captureTextUnavailable value)), ("slice", displayTree (captureTextSlice value))]
instance Display LookupLiteralMatch where
  displayTree value = Constructor "LookupLiteralMatch"
    [("match", displayTree (literalMatchSelection value)), ("context", displayTree (literalContextSelection value)),
     ("text", displayTree (literalMatchContext value))]
instance Display LookupLiteralSearch where
  displayTree value = Constructor "LookupLiteralSearch"
    [("source", displayTree (literalSource value)),
     ("batch", displayTree (literalBatchIdentity value)), ("view", displayTree (literalView value)),
     ("locator", displayTree (literalLocator value)), ("sourceIdentity", displayTree (literalSourceIdentity value)),
     ("unavailable", displayTree (literalUnavailable value)), ("count", displayTree (literalCount value)),
     ("matches", displayTree (literalMatches value)), ("omitted", displayTree (literalOmitted value))]
