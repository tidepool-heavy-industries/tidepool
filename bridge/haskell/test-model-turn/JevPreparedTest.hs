{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module JevPreparedTest (jevPreparedTests) where

import Prelude
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, modify, runState)
import qualified Data.Text as Text
import Test.Tasty.HUnit (assertBool, assertEqual)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Effects.Core (Jev (..), JevCallError (..))
import Tidepool.Inspection.Display (Display (..))
import qualified Jev.Operators as J
-- Keep the response-retaining public API example in this suite's compile gate.
import JevPreparedWorkflow ()

-- Successful response decoding belongs to the native codec gate. These host
-- checks exercise the generated typed failure path and the pure request view;
-- they do not replace Tidepool's native JSON anchors with a test codec.
jevPreparedTests :: TestTree
jevPreparedTests = testGroup "jev-prepared"
  (testCase "request inspection preserves original payload laziness" inspectPrepared
    : [testCase ("one host call retains " ++ show failure) (retainsFailure failure)
      | failure <- failures])
  where
    failures =
      [ JevUnconfigured, JevCallCap, JevTransport "unreachable", JevTimeout
      , JevHttp 503 "unavailable", JevBodyLimit, JevMalformed "invalid body"
      , JevCircuitOpen 503 987, JevClientSetup "setup refused"
      ]

inspectPrepared :: IO ()
inspectPrepared = do
  let world = J.state (#criteria J.:= ("input belongs to the prepared request" :: Text.Text))
      packet = #route J.:= J.choice "Which path matches the criteria?"
        (J.alt #first "First path" (error "first payload demanded" :: Text.Text)
          J..| J.alt #second "Second path" (error "second payload demanded" :: Text.Text))
  case J.prepare "named-model" world packet of
    Left failure -> error (show failure)
    Right prepared -> do
      let (preview, omitted) = displayWith 8192 prepared
      assertBool "prepared preview names its owner" ("Jev.Prepared" `Text.isInfixOf` preview)
      assertBool "model remains the one prepared" ("named-model" `Text.isInfixOf` preview)
      assertBool "original input can be inspected" ("input belongs to the prepared request" `Text.isInfixOf` preview)
      assertBool "question contract can be inspected" ("Which path matches the criteria?" `Text.isInfixOf` preview)
      assertBool "preview fits the declared budget" (not omitted)

retainsFailure :: JevCallError -> IO ()
retainsFailure failure = do
  let world = J.state (#input J.:= ("synthetic typed failure control" :: Text.Text))
      packet = #ready J.:= J.noul "Is the input ready?"
  case J.prepare "named-model" world packet of
    Left issue -> error (show issue)
    Right prepared -> do
      let (result, calls) = run (runState (0 :: Int)
            (interpret (reject failure) (J.executePrepared prepared)))
      assertEqual "one production Jev effect" 1 calls
      case result of
        Left actual -> assertEqual "typed host cause is preserved" (J.Transport failure) actual
        Right _ -> error "scripted host refusal unexpectedly succeeded"

reject :: JevCallError -> Jev a -> Eff '[State Int] a
reject failure (JevAskWith _) = modify (\count -> (count :: Int) + 1) >> pure (Left failure)
