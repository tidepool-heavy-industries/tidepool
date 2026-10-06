{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | The lookup tool's presentation policy. The Lookup effect owns resolution;
-- selection can only add bounded, explicitly requested related declarations.
module Tidepool.Lookup.Tools
  ( LookupTools (..), LookupArguments (..), Selection,
    tools, toolsWith, executeWith, packCandidates, rankCandidates,
    renderResult, candidateText,
  ) where

import Prelude hiding (lookup)
import Control.Monad.Freer (Eff, Member)
import Data.List (nubBy, sortBy)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.ByteString as BS
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Lookup

newtype LookupArguments = LookupArguments { queries :: [Text] }
  deriving (Generic, FromJSON, JsonSchema)

data LookupTools mode = LookupTools
  { lookup :: mode :- Call LookupArguments Text }
  deriving (Generic)

-- | The callback sees only bounded candidates and returns a subset to expand.
type Selection effects = [LookupResult] -> [LookupCandidate] -> Eff effects [LookupCandidate]

tools :: Member Lookup effects => LookupTools (AsServerT (Eff effects))
tools = toolFor (execute False (\_ _ -> pure []))

toolsWith :: Member Lookup effects => Selection effects -> LookupTools (AsServerT (Eff effects))
toolsWith select = toolFor (executeWith select)

toolFor :: (LookupArguments -> Eff effects Text) -> LookupTools (AsServerT (Eff effects))
toolFor action = LookupTools
  { lookup = presentWith id $ tool
      "Resolve missing API details before composing a cell: names, module exports, Haskell types or Exomonad documentation in this actor's scope. Pass {\"queries\":[\"Cmd.run\",\":: Int -> Int\",\"doc workbench\"]}; batch related questions. Search by input/output type when the name is unknown. Prefix type searches with :: and use _ for unknown parts. Qualified names are tried before module exports. During a pending request, queries \"respond\" and \"sessionInput\" reveal its exact reply and assignment types. Each query reports independently; narrow ambiguous or truncated matches. Availability labels describe the current effect row: a found signature alone does not establish that it is callable with your effects or runtime authority. When configured, related declarations and alternatives may be attached; explicitly look up a candidate for fuller detail. doc queries are text for this tool, not notebook Haskell."
      action
  }

executeWith :: Member Lookup effects => Selection effects -> LookupArguments -> Eff effects Text
executeWith = execute True

execute :: Member Lookup effects => Bool -> Selection effects -> LookupArguments -> Eff effects Text
execute discover select (LookupArguments names)
  | null names = pure "lookup requires at least one query"
  | otherwise = do
      original <- lookupRaw ((lookupRequest names) {lookupDiscover = discover})
      let primary = renderBatch original
          candidates = packCandidates (lookupCandidates original)
      if null candidates || lookupIssue original /= Nothing
        then pure primary
        else do
          proposed <- select (lookupResults original) candidates
          -- Even a custom selector cannot manufacture a query or lift the cap.
          let selected = take 4 (nubBy sameCandidate
                [ canonical | candidate <- proposed,
                  canonical <- candidates, sameCandidate candidate canonical ])
          if null selected
            then pure primary
            else do
              extra <- lookupRaw (LookupRequest [candidateQuery c | c <- selected, candidateReference c == Nothing] False
                (Just (lookupView original)) 0 [reference | c <- selected, Just reference <- [candidateReference c]])
              pure $ primary <> renderExtra selected extra

sameCandidate :: LookupCandidate -> LookupCandidate -> Bool
sameCandidate a b = candidateQuery a == candidateQuery b && candidateReference a == candidateReference b

-- | Candidate metadata, not full declarations, shares the scoring allowance.
-- Preserve the owner's fair/local-first ordering when applying the byte-free
-- Unicode scalar estimate (four scalars per estimated token).
packCandidates :: [LookupCandidate] -> [LookupCandidate]
packCandidates = go (8192 * 4) . take 128 . nubBy sameCandidate
  where
    go _ [] = []
    go remaining (candidate : rest) =
      let bounded = candidate { candidateSummary = T.pack (take 2048 (T.unpack (candidateSummary candidate))) }
          -- Reserve the repeated rubric, instructions, and wire keys as well as metadata.
          cost = length (T.unpack (candidateText bounded)) + 1024
      in if cost > remaining then go remaining rest
         else bounded : go (remaining - cost) rest

candidateText :: LookupCandidate -> Text
candidateText candidate =
  candidateQuery candidate <> maybe "" (\reference -> " (" <> T.pack (show (referenceNamespace reference)) <> ")") (candidateReference candidate) <> "\nFor queries: "
    <> T.intercalate ", " (candidateOrigins candidate)
    <> "\n" <> candidateSummary candidate

rankCandidates :: [(LookupCandidate, Double)] -> [LookupCandidate]
rankCandidates = take 4 . map fst . sortBy (\(_, a) (_, b) -> compare b a)
  . filter (\(_, score) -> score >= 2 && score <= 3)

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

renderExtra :: [LookupCandidate] -> LookupBatch -> Text
renderExtra selected batch = case lookupIssue batch of
  Just _ -> "\n\nRelated declarations unavailable; use explicit lookup to retry."
  Nothing ->
    let heading = "\n\nRelated declarations\n"
        recovery = recoveryText selected
        allowance = 2048 * 4 - scalarLength heading - scalarLength recovery
        (shown, omitted) = fit allowance (map addition (lookupResults batch))
    in if null shown && not omitted then ""
       else heading <> T.intercalate "\n\n" shown <> if omitted then recovery else ""
  where
    addition result =
      let origins = concat [candidateOrigins c | c <- selected, candidateQuery c == lookupQuery result]
      in renderResult result <> "\n  related to: " <> T.intercalate ", " origins
    fit _ [] = ([], False)
    fit budget (value : rest)
      | scalarLength value > budget =
          let (shown, _) = fit budget rest in (shown, True)
      | otherwise = let (shown, omitted) = fit (budget - scalarLength value - 2) rest
                    in (value : shown, omitted)

scalarLength :: Text -> Int
scalarLength = length . T.unpack

-- Exact namespace references remain recoverable even when a type and its
-- constructor have the same spelling. This is ordinary notebook Haskell.
recoveryText :: [LookupCandidate] -> Text
recoveryText selected =
  let queries' = [candidateQuery c | c <- selected, candidateReference c == Nothing]
      references = [reference | c <- selected, Just reference <- [candidateReference c]]
      expression = "lookupRaw (LookupRequest " <> T.pack (show queries')
        <> " False Nothing 0 " <> T.pack (show references) <> ")"
      full = "\nAdditional detail omitted. Raw notebook recovery: " <> expression
  in if scalarLength full <= 2048 then full
     else "\nAdditional detail omitted; narrow the original lookup or inspect its raw LookupBatch."
