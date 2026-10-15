{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- GHC-native oracle for the production tool policy. Judgments are supplied
-- directly; this does not exercise Jev's transport or the prepared engine.
module Main where

import Control.Monad (unless)
import Control.Monad.Freer (Eff, run)
import Control.Monad.Freer.Internal (handleRelay)
import qualified Data.Text as T
import Tidepool.Agent.Contract (AsServerT, ToolDispatchSuccess (..), compileTools, dispatch)
import Tidepool.Aeson.Value (object, toJSON, (.=))
import Tidepool.Lookup
import Tidepool.Effects.Core (Lookup (..), Console(..))
import qualified Tidepool.Lookup.Tools as Tools

candidates :: [LookupCandidate]
candidates = [LookupCandidate ("Fixture.Type" <> T.pack (show n)) ["root"]
  "data declaration" True Nothing | n <- [1..6 :: Int]]

entry :: T.Text -> LookupEntry
entry name = LookupEntry name (Just "Fixture") ("data " <> name)
  LookupType LookupAvailable LookupModuleExport LookupExact Nothing Nothing Nothing

original :: LookupBatch
original = LookupBatch [LookupResult "root" (LookupMissing ["root"] [])]
  candidates "frozen-view" Nothing "fixture-capture" (LookupSearchReceipt "frozen-view" LookupSearchCaptured [] [] (LookupDiscoveryNotRun LookupSearchUnavailable))

answer :: LookupRequest -> LookupBatch
answer request
  | lookupDiscover request = original
  | otherwise = LookupBatch
      [LookupResult query (LookupFound [entry query] False) | query <- lookupQueries request]
      [] "frozen-view" Nothing "fixture-capture" (LookupSearchReceipt "frozen-view" LookupSearchCaptured [] [] (LookupDiscoveryNotRun LookupSearchUnavailable))

observe :: Eff '[Lookup] a -> ([LookupRequest], a)
observe = observeWith answer

observeWith :: (LookupRequest -> LookupBatch) -> Eff '[Lookup] a -> ([LookupRequest], a)
observeWith respond = run . observeLookup respond

observeLookup :: (LookupRequest -> LookupBatch) -> Eff (Lookup ': effects) a -> Eff effects ([LookupRequest], a)
observeLookup respond = handleRelay (\value -> pure ([], value))
  (\(LookupRaw request) resume -> do
    (requests, value) <- resume (respond request)
    pure (request : requests, value))

observeHosted :: Eff '[Lookup, Console] a -> ([LookupRequest], a)
observeHosted = run . handleRelay pure (\request resume -> case request of
  DisplayAllowanceWith -> resume 8192
  DisplaySelectableWith _ _ -> resume (1,1,1)
  _ -> error "unexpected hosted lookup presentation effect") . observeLookup answer

check :: String -> Bool -> IO ()
check name value = unless value (fail name)

main :: IO ()
main = do
  let expectedDefaults = LookupRequest ["Cmd.quiet"] False Nothing 128 []
      (cellRequests, _) = observe (lookupRaw (lookupRequest ["Cmd.quiet"]))
      hostedTools = Tools.tools :: Tools.LookupTools (AsServerT (Eff '[Lookup, Console]))
      hosted = either (error . show) id (compileTools hostedTools)
      (hostedRequests, hostedResult) = observeHosted
        (dispatch hosted "lookup" (object ["queries" .= (["Cmd.quiet"] :: [T.Text])]))
  check "cell lookup constructor matches the shipped hosted tool request"
    (cellRequests == [expectedDefaults]
      && hostedRequests == [expectedDefaults]
      && lookupRequest ["Cmd.quiet"] == expectedDefaults)
  check "hosted lookup returns retained selection identity"
    (case hostedResult of
      Right (ToolDispatchSuccess value _) -> value == toJSON (Tools.LookupCaptureReceipt 1 1 1 "frozen-view")
      _ -> False)
  let select _ values = pure (Tools.rankCandidates (zip values [2,3,1,3,2,3]))
      (requests, output) = observe (Tools.executeWith select (Tools.LookupArguments ["root"]))
  check "one original batch and one nonrecursive follow-up" (length requests == 2)
  check "selected whole declarations with deterministic score order"
    (lookupQueries (requests !! 1) == ["Fixture.Type2", "Fixture.Type4", "Fixture.Type6", "Fixture.Type1"])
  check "frozen view and raw follow-up"
    (lookupExpectedView (requests !! 1) == Just "frozen-view"
      && not (lookupDiscover (requests !! 1)))
  check "original miss retained with bounded additions"
    ("no match:" `T.isInfixOf` output && T.count "related to:" output == 4)
  let (none, unchanged) = observe
        (Tools.executeWith (\_ _ -> pure []) (Tools.LookupArguments ["root"]))
  check "abstention preserves original without a follow-up"
    (length none == 1 && not ("Related declarations" `T.isInfixOf` unchanged))
  let fabricated = LookupCandidate "invented" ["root"] "invented" True Nothing
      (guarded, guardedOutput) = observe (Tools.executeWith
        (\_ values -> pure (fabricated : map (\candidate -> candidate {candidateOrigins = ["forged"]}) values ++ values)) (Tools.LookupArguments ["root"]))
  check "custom selector cannot invent a lookup or lift the cap"
    (lookupQueries (guarded !! 1) == map candidateQuery (take 4 candidates))
  check "selector metadata is canonicalized before display"
    (not ("forged" `T.isInfixOf` guardedOutput))
  let oversized request
        | lookupDiscover request = original
        | otherwise = (answer request) {lookupResults =
            [LookupResult query (LookupFound
              [(entry query) {lookupDeclaration = T.replicate 10000 "x"}] False)
            | query <- lookupQueries request]}
      (_, bounded) = observeWith oversized
        (Tools.executeWith select (Tools.LookupArguments ["root"]))
  check "oversized additions leave bounded recovery names"
    ("Fixture.Type2" `T.isInfixOf` bounded
      && "lookupRaw (LookupRequest" `T.isInfixOf` bounded && T.length bounded < 8192)
  let example source = LookupExample "checks/example.hs" "Import Cmd"
        ["Project/Helper.hs"] source (Just "captured-main") Nothing []
      exampleAnswer source request = LookupBatch
        [LookupResult query (LookupFound
          [(entry query) {lookupExample = Just (example source)}] False)
        | query <- lookupQueries request] [] "frozen-view" Nothing "fixture-capture" (LookupSearchReceipt "frozen-view" LookupSearchCaptured [] [] (LookupDiscoveryNotRun LookupSearchUnavailable))
      (_, twoExamples) = observeWith (exampleAnswer "Cmd.start command\n")
        (Tools.executeWith (\_ _ -> pure []) (Tools.LookupArguments ["first", "second"]))
  check "only first direct hit carries one complete example"
    (T.count "```haskell" twoExamples == 1
      && T.count "Prerequisites: Project/Helper.hs" twoExamples == 1
      && "Cmd.start command" `T.isInfixOf` twoExamples)
  let relatedExample request
        | lookupDiscover request = original
        | otherwise = LookupBatch
            [LookupResult query (LookupFound
              [(entry query) {lookupExample = Just (example "Cmd.start command\n")}] False)
            | query <- lookupQueries request] [] "frozen-view" Nothing "fixture-capture" (LookupSearchReceipt "frozen-view" LookupSearchCaptured [] [] (LookupDiscoveryNotRun LookupSearchUnavailable))
      (_, relatedOutput) = observeWith relatedExample
        (Tools.executeWith select (Tools.LookupArguments ["root"]))
  check "related declarations do not display examples"
    ("Related declarations" `T.isInfixOf` relatedOutput
      && not ("```haskell" `T.isInfixOf` relatedOutput))
  let unicodeSource = T.replicate 700 "😀"
      (_, omittedExample) = observeWith (exampleAnswer unicodeSource)
        (Tools.executeWith (\_ _ -> pure []) (Tools.LookupArguments ["first"]))
  check "oversized UTF-8 example retains complete source and requirements"
    (unicodeSource `T.isInfixOf` omittedExample
      && "checks/example.hs" `T.isInfixOf` omittedExample
      && "Import Cmd" `T.isInfixOf` omittedExample
      && "```haskell" `T.isInfixOf` omittedExample)
  let unicodeRequirements = T.replicate 120 "😀"
      longExample = (example unicodeSource) {exampleRequirements = unicodeRequirements}
      withLongMetadata request = LookupBatch
        [LookupResult query (LookupFound
          [(entry query) {lookupExample = Just longExample}] False)
        | query <- lookupQueries request] [] "frozen-view" Nothing "fixture-capture" (LookupSearchReceipt "frozen-view" LookupSearchCaptured [] [] (LookupDiscoveryNotRun LookupSearchUnavailable))
      (_, boundedMetadata) = observeWith withLongMetadata
        (Tools.executeWith (\_ _ -> pure []) (Tools.LookupArguments ["first"]))
  check "multibyte requirements and complete source survive rendering"
    (unicodeRequirements `T.isInfixOf` boundedMetadata
      && "Project/Helper.hs" `T.isInfixOf` boundedMetadata
      && unicodeSource `T.isInfixOf` boundedMetadata)
  let maximal = (example unicodeSource)
        { exampleLocator = T.replicate 256 "x"
        , exampleRequirements = T.replicate 256 "😀"
        , examplePrerequisites = [T.replicate 250 "p", T.replicate 250 "q"] }
      maximalAnswer request = LookupBatch
        [LookupResult query (LookupFound
          [(entry query) {lookupExample = Just maximal}] False)
        | query <- lookupQueries request] [] "frozen-view" Nothing "fixture-capture" (LookupSearchReceipt "frozen-view" LookupSearchCaptured [] [] (LookupDiscoveryNotRun LookupSearchUnavailable))
      (_, maximalOutput) = observeWith maximalAnswer
        (Tools.executeWith (\_ _ -> pure []) (Tools.LookupArguments ["first"]))
      (_, maximalBlock) = T.breakOn "\n  Example: " maximalOutput
  check "complete captured source survives large metadata"
    (unicodeSource `T.isInfixOf` maximalBlock && not ("Code omitted" `T.isInfixOf` maximalBlock))
  putStrLn "lookup policy oracle passed (14 checks)"
