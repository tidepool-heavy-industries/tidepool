{-# LANGUAGE OverloadedStrings #-}

-- | Pure presentation shared by the hosted tool and retained workbench values.
module Tidepool.Lookup.Render (renderBatch, renderResult) where

import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.ByteString as BS
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

-- Keep the complete executable source or give its exact locator. The 2 KiB
-- allowance includes every label, locator, prerequisite, and UTF-8 byte.
renderExample :: LookupExample -> Text
renderExample example =
  let heading = "\n  Example: " <> exampleLocator example
        <> "\n  Requirements: " <> exampleRequirements example
      prerequisites = if null (examplePrerequisites example) then ""
        else "\n  Prerequisites: " <> T.intercalate ", " (examplePrerequisites example)
      source = exampleSource example
      full = heading <> prerequisites <> "\n```haskell\n" <> source
        <> (if T.isSuffixOf "\n" source then "" else "\n") <> "```"
  in if BS.length (TE.encodeUtf8 full) <= 2048 then full
     else heading <> prerequisites
       <> "\n  Code omitted (example exceeds 2 KiB); open the locator for the complete source."

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

