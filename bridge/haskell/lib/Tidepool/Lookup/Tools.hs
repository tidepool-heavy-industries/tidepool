{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | The lookup tool's presentation policy. The Lookup effect owns resolution;
-- selection can only add bounded, explicitly requested related declarations.
module Tidepool.Lookup.Tools
  ( LookupTools (..), LookupArguments (..), LookupReadArguments (..), LookupNavigation (..), Selection,
    tools, toolsWith, executeWith, packCandidates, rankCandidates,
    renderBatch, renderResult, candidateText,
  ) where

import Prelude hiding (lookup)
import Control.Monad.Freer (Eff, Member)
import Data.List (nubBy, sortBy)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON, eitherDecode)
import Tidepool.Aeson.Value (ToJSON, toJSON, encodeValue)
import Tidepool.Inspection (displaySelectable, selectDisplay, displayIdentity, displayTree, DisplayTree(..))
import Tidepool.Lookup.Navigation
import Tidepool.Effects.Core (Console)
import Tidepool.Agent.Contract
import Tidepool.Lookup
import Tidepool.Lookup.Render (renderBatch, renderResult)

newtype LookupArguments = LookupArguments { queries :: [Text] }
  deriving (Generic, FromJSON, JsonSchema)

data LookupNavigation = LookupIndex | LookupSections [LookupSelection]
  deriving (Generic, FromJSON, ToJSON, JsonSchema)
data LookupReadArguments = LookupReadArguments
  { retainedActor :: Int, retainedIncarnation :: Int, retainedSlot :: Int
  , requested :: LookupNavigation
  } deriving (Generic, FromJSON, JsonSchema)

data LookupTools mode = LookupTools
  { lookup :: mode :- Call LookupArguments Text
  , readLookup :: mode :- Call LookupReadArguments Text
  }
  deriving (Generic)

-- | The callback sees only bounded candidates and returns a subset to expand.
type Selection effects = [LookupResult] -> [LookupCandidate] -> Eff effects [LookupCandidate]

tools :: (Member Lookup effects, Member Console effects) => LookupTools (AsServerT (Eff effects))
tools = toolFor (executeRetained False (\_ _ -> pure []))

toolsWith :: (Member Lookup effects, Member Console effects) => Selection effects -> LookupTools (AsServerT (Eff effects))
toolsWith select = toolFor (executeRetained True select)

toolFor :: Member Console effects => (LookupArguments -> Eff effects Text) -> LookupTools (AsServerT (Eff effects))
toolFor action = LookupTools
  { lookup = presentWith id $ tool
      "Resolve missing API details before composing a cell: names, module exports, Haskell types or Exomonad documentation in this actor's scope. Pass {\"queries\":[\"Cmd.run\",\":: Int -> Int\",\"doc workbench\"]}; batch related questions. Search by input/output type when the name is unknown. Prefix type searches with :: and use _ for unknown parts. Qualified names are tried before module exports. During a pending request, queries \"respond\" and \"sessionInput\" reveal its exact reply and assignment types. Each query reports independently; narrow ambiguous or truncated matches. Availability labels describe the current effect row: a found signature alone does not establish that it is callable with your effects or runtime authority. When configured, related declarations and alternatives may be attached; explicitly look up a candidate for fuller detail. doc queries are text for this tool, not notebook Haskell."
      action
  , readLookup = presentWith id $ tool
      "Read the retained lookup by actor/incarnation/slot from its query result. Use LookupIndex to revisit the index, or LookupSections with stable result/entry ordinals, named sections and optional 1-based inclusive line ranges. Selection preserves order and never refetches."
      (\request -> do
        _ <- selectDisplay (retainedActor request, retainedIncarnation request, retainedSlot request)
          (encodeValue (toJSON (requested request)))
        pure "Read from the original retained lookup; source was not fetched again.")
  }

executeWith :: Member Lookup effects => Selection effects -> LookupArguments -> Eff effects Text
executeWith = execute True

execute :: Member Lookup effects => Bool -> Selection effects -> LookupArguments -> Eff effects Text
execute discover select (LookupArguments names)
  | null names = pure "lookup requires at least one query"
  | otherwise = do
      (original, related) <- captureLookup discover select names
      pure (renderBatch original <> maybe "" (uncurry renderExtra) related)

-- The same resolution policy feeds text compatibility and retained navigation.
-- Navigation retains the result of this function, never this effectful action.
captureLookup :: Member Lookup effects => Bool -> Selection effects -> [Text] -> Eff effects (LookupBatch, Maybe ([LookupCandidate], LookupBatch))
captureLookup discover select names = do
  original <- lookupRaw ((lookupRequest names) {lookupDiscover = discover})
  let candidates = packCandidates (lookupCandidates original)
  if null candidates || lookupIssue original /= Nothing
    then pure (original, Nothing)
    else do
      proposed <- select (lookupResults original) candidates
      let selected = take 4 (nubBy sameCandidate
            [canonical | candidate <- proposed, canonical <- candidates, sameCandidate candidate canonical])
      if null selected then pure (original, Nothing) else do
        extra <- lookupRaw (LookupRequest [candidateQuery c | c <- selected, candidateReference c == Nothing] False
          (Just (lookupView original)) 0 [reference | c <- selected, Just reference <- [candidateReference c]])
        pure (original, Just (selected, extra))

executeRetained :: (Member Lookup effects, Member Console effects) => Bool -> Selection effects -> LookupArguments -> Eff effects Text
executeRetained discover select (LookupArguments names)
  | null names = pure "lookup requires at least one query"
  | otherwise = do
      (original, related) <- captureLookup discover select names
      let captured = case related of
            Nothing -> original
            Just (_, extra) -> original
              { lookupIdentity = lookupIdentity original <> "+" <> lookupIdentity extra
              , lookupResults = lookupResults original ++ lookupResults extra
              , lookupIssue = case lookupIssue extra of Nothing -> lookupIssue original; issue -> issue }
          index = TextLeaf (renderLookupIndex captured)
          navigate payload = case eitherDecode payload of
            Left issue -> TextLeaf ("Invalid lookup selection: " <> issue)
            Right LookupIndex -> index
            Right (LookupSections selections) -> displayTree (selectLookupMany captured selections)
      handle <- displaySelectable index navigate
      let (actor, incarnation, slot) = displayIdentity handle
      pure ("Lookup retained in view " <> lookupView captured
        <> "; read_lookup with retainedActor=" <> T.pack (show actor)
        <> ", retainedIncarnation=" <> T.pack (show incarnation)
        <> ", retainedSlot=" <> T.pack (show slot)
        <> ". Named sections and line ranges read this exact capture; LookupIndex returns to the index."
        <> maybe "" ("\nLookup unavailable: " <>) (lookupIssue captured))

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
