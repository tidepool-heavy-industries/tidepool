{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE OverloadedRecordDot #-}

module LookupFixture where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import qualified Tidepool.Command as Cmd
import Tidepool.Effects (lookupRaw)
import Tidepool.Effects.Core
import qualified Tidepool.Lookup.Tools as Tools
import qualified Project.Lookup as Project
import qualified Jev.Operators as J
import Tidepool.Aeson.FromJSON (eitherDecode)
import Tidepool.Aeson.Value (Value)

data First = First HiddenSecondDegree
data Second = Second
data Third = Third
data Fourth = Fourth
data Fifth = Fifth
data Sixth = Sixth
data HiddenSecondDegree = HiddenSecondDegree

relatedRoot :: First -> Second -> Third -> Fourth -> Fifth -> Sixth -> ()
relatedRoot _ _ _ _ _ _ = ()

alternativeRoot :: First -> ()
alternativeRoot _ = ()

candidate :: T.Text -> LookupCandidate
candidate name = LookupCandidate name ["root"] "brief" True Nothing

-- Ties retain source order; out-of-rubric values must not outrank evidence.
rankingCheck :: Bool
rankingCheck = map Tools.candidateOrdinal (Project.rankCandidates (zip
  (Tools.packingCandidates (Tools.packCandidates "immutable-view" (map candidate
    ["low", "first", "second", "invalid", "nan", "top", "third", "fourth"])))
  [1.99, 2, 2, 4, 0 / 0, 3, 2, 2])) == [6, 2, 3, 7]

-- Supplementary Unicode scalars count as one, and oversized metadata cannot
-- crowd out a later small candidate or defeat the aggregate scoring budget.
packingCheck :: Bool
packingCheck =
  let unicode = (candidate "unicode") {candidateSummary = T.replicate 3000 "😀"}
      tooLarge = candidate (T.replicate 40000 "x")
      packed = packedValues [unicode, unicode, tooLarge, candidate "small"]
      many = packedValues
        [(candidate (T.pack (show n))) {candidateSummary = T.replicate 2048 "😀"}
        | n <- [1..200 :: Int]]
  in map candidateQuery packed == ["unicode", "small"]
    && map (length . T.unpack . candidateSummary) packed == [2048, 5]
    && sum (map (length . T.unpack . Tools.candidateText) many) <= 8192 * 4
    && length many <= 128
    && length (packedValues
      [ (candidate "Second") {candidateReference = Just
          (LookupReference "LookupFixture" "Second" LookupTypeNamespace)}
      , (candidate "Second") {candidateReference = Just
          (LookupReference "LookupFixture" "Second" LookupConstructorNamespace)}
      ]) == 2

packedValues :: [LookupCandidate] -> [LookupCandidate]
packedValues = map Tools.packedCandidate . Tools.packingCandidates . Tools.packCandidates "immutable-view"

rawLookupCheck :: Member Lookup effects => Eff effects Bool
rawLookupCheck = do
  result <- lookupRaw (LookupRequest ["LookupFixture.relatedRoot"] False Nothing 0 [])
  pure (lookupIssue result == Nothing && null (lookupCandidates result)
    && case lookupResults result of
      [LookupResult _ (LookupFound _ _)] -> True
      _ -> False)

exampleIdentityCheck :: Member Lookup effects => Eff effects Bool
exampleIdentityCheck = do
  result <- lookupRaw (LookupRequest ["Cmd.start"] False Nothing 0 [])
  pure (case lookupResults result of
    [LookupResult _ (LookupFound [entry] False)] ->
      lookupModule entry == Just "Tidepool.Command"
        && lookupOrigin entry == LookupModuleExport
        && maybe False ((== ".exomonad/checks/background-command-example.hs") . exampleLocator)
             (lookupExample entry)
    _ -> False)

renderedExampleCheck :: Member Lookup effects => Eff effects Bool
renderedExampleCheck = do
  rendered <- Tools.renderCapture <$> Tools.captureWith (\_ _ -> pure (Tools.SelectionSucceeded () []))
    (Tools.LookupArguments ["Cmd.start", "Cmd.start"])
  captured <- lookupRaw (LookupRequest ["Cmd.start"] False Nothing 0 [])
  pure (case lookupResults captured of
    [LookupResult _ (LookupFound [entry] False)] -> case lookupExample entry of
      Just example -> exampleSourceIssue example == Nothing
        && not (T.null (exampleSource example))
        && T.count (exampleSource example) rendered == 1
        && T.count "```haskell" rendered == 1
      Nothing -> False
    _ -> False)

emptySelectionCheck :: (Member Jev effects, Member Reflect effects) => Eff effects Bool
emptySelectionCheck = do
  outcome <- Project.select [] []
  pure (case outcome of Tools.SelectionSkipped Tools.NoCandidates -> True; _ -> False)

-- The type and constructor have the same spelling but distinct compiler IDs.
namespaceCheck :: Member Lookup effects => Eff effects Bool
namespaceCheck = do
  result <- lookupRaw (LookupRequest [] False Nothing 0
    [ LookupReference "LookupFixture" "Second" LookupTypeNamespace
    , LookupReference "LookupFixture" "Second" LookupConstructorNamespace
    , LookupReference "" "Just" LookupConstructorNamespace ])
  pure (lookupIssue result == Nothing && case lookupResults result of
    [LookupResult _ (LookupFound [typeEntry] _), LookupResult _ (LookupFound [constructorEntry] _),
     LookupResult _ (LookupFound [scopeEntry] _)] ->
      lookupKind typeEntry == LookupType && lookupKind constructorEntry == LookupConstructor
        && lookupKind scopeEntry == LookupConstructor
    _ -> False)

captureEvidence :: (Member Lookup effects, Member Jev effects, Member Reflect effects)
  => Eff effects (Tools.LookupCapture Project.SelectionEvidence)
captureEvidence = Project.capture (Tools.LookupArguments ["LookupFixture.relatedRoot"])

-- This equality must run on the prepared runtime, whose JSON text decoder is
-- real. The native GHC tests deliberately do not claim transport-byte evidence.
recordingMatches :: Tools.LookupCapture Project.SelectionEvidence -> T.Text -> Bool
recordingMatches captured actualWire = case (Tools.captureSelection captured, eitherDecode actualWire :: Either T.Text Value) of
  (Tools.SelectionSucceeded evidence _, Right request) ->
    case (J.recordings (Project.selectionEvaluation evidence), J.result (Project.selectionEvaluation evidence),
          J.observations (Project.selectionEvaluation evidence)) of
      ([recording], Right scores, [observation]) -> J.recordedRequest recording == request
        && length scores == length (Tools.packingCandidates (Tools.capturePacking captured))
        && all (\score -> (Project.scoreRelevance score).expectation == 3
          && (Project.scoreRelevance score).confidence == 1
          && sum (map snd ((Project.scoreRelevance score).masses)) == 1) scores
        && case J.observationResult observation of
          Right response -> J.resolvedModel response == "jev-test"
          Left _ -> False
      _ -> False
  _ -> False
