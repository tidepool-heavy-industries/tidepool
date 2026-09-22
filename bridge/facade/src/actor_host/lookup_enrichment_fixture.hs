{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module LookupFixture where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Tidepool.Effects (lookupRaw)
import Tidepool.Effects.Core
import qualified Tidepool.Lookup.Tools as Tools
import qualified Project.Lookup as Project

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
rankingCheck = map candidateQuery (Tools.rankCandidates
  [(candidate "low", 1.99), (candidate "first", 2),
   (candidate "second", 2), (candidate "invalid", 4),
   (candidate "nan", 0 / 0), (candidate "top", 3),
   (candidate "third", 2), (candidate "fourth", 2)])
  == ["top", "first", "second", "third"]

-- Supplementary Unicode scalars count as one, and oversized metadata cannot
-- crowd out a later small candidate or defeat the aggregate scoring budget.
packingCheck :: Bool
packingCheck =
  let unicode = (candidate "unicode") {candidateSummary = T.replicate 3000 "😀"}
      tooLarge = candidate (T.replicate 40000 "x")
      packed = Tools.packCandidates [unicode, unicode, tooLarge, candidate "small"]
      many = Tools.packCandidates
        [(candidate (T.pack (show n))) {candidateSummary = T.replicate 2048 "😀"}
        | n <- [1..200 :: Int]]
  in map candidateQuery packed == ["unicode", "small"]
    && map (length . T.unpack . candidateSummary) packed == [2048, 5]
    && sum (map (length . T.unpack . Tools.candidateText) many) <= 8192 * 4
    && length many <= 128
    && length (Tools.packCandidates
      [ (candidate "Second") {candidateReference = Just
          (LookupReference "LookupFixture" "Second" LookupTypeNamespace)}
      , (candidate "Second") {candidateReference = Just
          (LookupReference "LookupFixture" "Second" LookupConstructorNamespace)}
      ]) == 2

rawLookupCheck :: Member Lookup effects => Eff effects Bool
rawLookupCheck = do
  result <- lookupRaw (LookupRequest ["LookupFixture.relatedRoot"] False Nothing 0 [])
  pure (lookupIssue result == Nothing && null (lookupCandidates result)
    && case lookupResults result of
      [LookupResult _ (LookupFound _ _)] -> True
      _ -> False)

emptySelectionCheck :: (Member Jev effects, Member Reflect effects) => Eff effects Bool
emptySelectionCheck = null <$> Project.select [] []

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
