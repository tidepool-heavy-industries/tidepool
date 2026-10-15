{-# LANGUAGE OverloadedStrings #-}

-- | Pure presentation shared by the hosted tool and retained workbench values.
module Tidepool.Lookup.Render (renderBatch, renderResult) where

import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Lookup

renderBatch :: LookupBatch -> Text
renderBatch batch = T.intercalate "\n\n" (go False (lookupResults batch))
  <> maybe "" ("\nLookup unavailable: " <>) (lookupIssue batch)
  where
    go _ [] = []
    go used (result : rest) =
      let example = if used then Nothing else case lookupOutcome result of
            LookupFound [entry] False -> lookupExample entry
            _ -> Nothing
          rendered = renderResult result <> maybe "" renderExample example
      in rendered : go (used || maybe False (const True) example) rest

-- Full source is retained; the Display owner applies output budgets.
renderExample :: LookupExample -> Text
renderExample example =
  let heading = "\n  Example: " <> exampleLocator example
        <> "\n  Requirements: " <> exampleRequirements example
      prerequisites = if null (examplePrerequisites example) then ""
        else "\n  Prerequisites: " <> T.intercalate ", " (examplePrerequisites example)
      source = exampleSource example
      full = heading <> prerequisites <> "\n```haskell\n" <> source
        <> (if T.isSuffixOf "\n" source then "" else "\n") <> "```"
  in maybe full (\issue -> heading <> prerequisites <> "\n  Source not retained: " <> issue) (exampleSourceIssue example)

renderResult :: LookupResult -> Text
renderResult result = lookupQuery result <> "\n" <> case lookupOutcome result of
  LookupFound entries omitted -> renderEntries entries omitted
  LookupAmbiguous entries omitted -> "  ambiguous:\n" <> renderEntries entries omitted
  LookupMissing attempted suggestions -> "  no match: " <> T.intercalate ", " attempted
    <> if null suggestions then "" else "; close: " <> T.intercalate ", " suggestions
  LookupRejected diagnostic -> "  error: " <> T.strip diagnostic

renderEntries :: [LookupEntry] -> Bool -> Text
renderEntries entries omitted = T.intercalate "\n" (map renderEntry entries)
  <> if omitted then "\n  … more matches omitted; narrow the lookup" else ""

renderEntry :: LookupEntry -> Text
renderEntry entry = "  " <> availability <> T.strip (lookupDeclaration entry)
  <> maybe "" (\pointer -> " (see: " <> pointer <> ")") (lookupUsage entry)
  <> maybe "" (\contract -> "\n  Contract: " <> T.strip contract) (lookupContract entry)
  where
    availability = case lookupKind entry of
      LookupValue -> label
      LookupClassMethod -> label
      LookupRecordSelector -> label
      _ -> ""
    label = "[" <> (case lookupAvailability entry of
      LookupAvailable -> "available"
      LookupPolymorphic -> "polymorphic"
      LookupUnknown -> "unknown"
      LookupUnavailable -> "unavailable") <> "] "

