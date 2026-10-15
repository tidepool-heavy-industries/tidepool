{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
module LookupEvidenceTest (lookupEvidenceTests) where

import Prelude
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, modify, runState)
import qualified Data.Text as T
import Data.List (sortOn)
import Test.Tasty.QuickCheck (testProperty)
import Test.Tasty.HUnit (assertBool, assertEqual)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Effects.Core hiding (run, error)
import Tidepool.Inspection (displayWith)
import Tidepool.Aeson.Value (Value(..))
import qualified Tidepool.Aeson.KeyMap as KM
import qualified Tidepool.Lookup.Tools as Lookup
import qualified Project.Lookup as Project
import qualified Jev.Operators as J

-- Host tests preserve typed refusals through the production Jev effect.
-- Successful JSON response decoding is exercised by the prepared facade gate.
lookupEvidenceTests :: TestTree
lookupEvidenceTests = testGroup "lookup judgment evidence"
  [ testProperty "ranking agrees with independent stable rubric oracle" $ \(numbers :: [Int]) ->
      let values = [LookupCandidate (T.pack (show n)) ["root"] "brief" True Nothing | n <- [1..min 25 (length numbers)]]
          packed = Lookup.packingCandidates (Lookup.packCandidates "captured-view" values)
          scored = zip packed (map (\n -> fromIntegral (n `mod` 11) / 2) numbers)
          expected = take 4 [ordinal | (_, ordinal) <- sortOn id
            [(negate score, Lookup.packedOrdinal candidate) | (candidate, score) <- scored, score >= 2, score <= 3]]
      in map Lookup.candidateOrdinal (Project.rankCandidates scored) == expected
  , testCase "rubric threshold NaN invalid values and ties retain source order" $ do
      let values = [LookupCandidate (T.pack (show n)) ["root"] "brief" True Nothing | n <- [1..8 :: Int]]
          packed = Lookup.packingCandidates (Lookup.packCandidates "captured-view" values)
      assertEqual "only useful/direct are selected, best first then source order" [6,3,5,7]
        (map Lookup.candidateOrdinal (Project.rankCandidates (zip packed [0/0, 1.999, 2, 4, 2, 3, 2, 3.001])))
  , testCase "empty pool skips both reflection and judgment" $ do
      let outcome = run (interpret unexpectedReflect (interpret unexpectedJev (Project.select [] [])))
      case outcome of
        Lookup.SelectionSkipped Lookup.NoCandidates -> pure ()
        _ -> error "empty pool did not preserve skipped outcome"
  , testCase "typed judgment failure retains exact input pool and evaluation" $ do
      let original = LookupResult "root" (LookupMissing ["qualified.root", "root"] ["alternative"])
          candidate = LookupCandidate "Owner.alternative" ["root"] (T.replicate 3000 "🙂") True Nothing
          packed = Lookup.packingCandidates (Lookup.packCandidates "captured-view" [candidate])
          (outcome, requests) = run (runState [] (interpret noHistory
            (interpret refuse (Project.select [original] packed))))
      assertEqual "one actual Jev call" 1 (length requests)
      case outcome of
        Lookup.SelectionFailed evidence -> do
          assertBool "exact original lookup and reflection status retained" (case Project.selectionInput evidence of
            _ -> let (shown, _) = displayWith 65536 evidence in "root" `T.isInfixOf` shown
              && "conversation history unavailable" `T.isInfixOf` shown)
          case J.result (Project.selectionEvaluation evidence) of
            Left (J.CallFailed (J.Transport JevUnconfigured)) -> pure ()
            _ -> error "typed Jev failure changed or became successful empty"
          case J.recordings (Project.selectionEvaluation evidence) of
            [recording] -> do
              assertEqual "recording owns exact input Value" (Just (Project.selectionInput evidence))
                (field "state" (J.recordedRequest recording))
              assertBool "one bounded candidate has its stable question key" (case field "questions" (J.recordedRequest recording) of
                Just (Object questions) -> KM.keys questions == ["0.candidates.c1.relevance"]
                _ -> False)
              assertEqual "typed host cause survives recording" (Left JevUnconfigured) (J.recordedOutcome recording)
            _ -> error "failure observation missing"
          assertEqual "original candidate summary survives bounded judgment" candidate (Lookup.packedOriginal (head packed))
          assertEqual "exact bounded candidate scored once" 2048 (length (T.unpack (candidateSummary (Lookup.packedCandidate (head packed)))))
        _ -> error "Jev refusal was not a failed selection"
  ]
  where
    unexpectedReflect :: Reflect a -> Eff '[] a
    unexpectedReflect _ = error "empty lookup performed reflection"
    unexpectedJev :: Jev a -> Eff '[Reflect] a
    unexpectedJev _ = error "empty lookup performed judgment"
    noHistory :: Reflect a -> Eff '[State [T.Text]] a
    noHistory (ReflectWith count) = if count == 8 then pure (Left ReflectUnbound) else error "reflection bound changed"
    refuse :: Jev a -> Eff '[Reflect, State [T.Text]] a
    refuse (JevAskWith body) = modify (++ [body]) >> pure (Left JevUnconfigured)

field :: T.Text -> Value -> Maybe Value
field key (Object fields) = KM.lookup (KM.fromText key) fields
field _ _ = Nothing
