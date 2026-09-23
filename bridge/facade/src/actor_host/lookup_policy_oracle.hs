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
import Tidepool.Lookup
import Tidepool.Effects.Core (Lookup (..))
import qualified Tidepool.Lookup.Tools as Tools

candidates :: [LookupCandidate]
candidates = [LookupCandidate ("Fixture.Type" <> T.pack (show n)) ["root"]
  "data declaration" True Nothing | n <- [1..6 :: Int]]

entry :: T.Text -> LookupEntry
entry name = LookupEntry name (Just "Fixture") ("data " <> name)
  LookupType LookupAvailable LookupModuleExport LookupExact Nothing

original :: LookupBatch
original = LookupBatch [LookupResult "root" (LookupMissing ["root"] [])]
  candidates "frozen-view" Nothing

answer :: LookupRequest -> LookupBatch
answer request
  | lookupDiscover request = original
  | otherwise = LookupBatch
      [LookupResult query (LookupFound [entry query] False) | query <- lookupQueries request]
      [] "frozen-view" Nothing

observe :: Eff '[Lookup] T.Text -> ([LookupRequest], T.Text)
observe = observeWith answer

observeWith :: (LookupRequest -> LookupBatch) -> Eff '[Lookup] T.Text -> ([LookupRequest], T.Text)
observeWith respond = run . handleRelay (\value -> pure ([], value))
  (\(LookupRaw request) resume -> do
    (requests, value) <- resume (respond request)
    pure (request : requests, value))

check :: String -> Bool -> IO ()
check name value = unless value (fail name)

main :: IO ()
main = do
  let expectedDefaults = lookupRequest ["Cmd.quiet"]
      (cellRequests, _) = observe (lookupRaw (lookupRequest ["Cmd.quiet"]))
  check "cell lookup constructor uses hosted defaults"
    (cellRequests == [expectedDefaults]
      && lookupQueries expectedDefaults == ["Cmd.quiet"]
      && lookupDiscover expectedDefaults
      && lookupExpectedView expectedDefaults == Nothing
      && lookupCandidateLimit expectedDefaults == 128
      && null (lookupReferences expectedDefaults))
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
  putStrLn "lookup policy oracle passed (8 checks)"
