{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

module ResearcherSpecTest (researcherSpecTests) where

import Prelude
import Control.Monad.Freer (Eff, interpret, run)
import qualified Data.Map.Strict as Map
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import Test.Tasty.HUnit (assertEqual, assertBool, assertFailure)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Aeson
import Tidepool.Agent.Contract (JsonSchema (..), prepareAgentSpec, preparedAgentSpecMetadata, presentJson)
import Tidepool.Agent.ToolEffects (toolEffectNames)
import Tidepool.Effects.Core (Web (..), WebError (..))
import Tidepool.Inspection.Display (Display (..), WorkbenchDisplay (..))
import qualified Project.Researcher as Researcher

field :: Text -> Value -> Value
field name (Object fields) = case Map.lookup name fields of
  Just value -> value
  Nothing -> error ("missing field: " ++ show name)
field _ value = error ("expected object: " ++ show value)

researcherSpecTests :: TestTree
researcherSpecTests = testGroup "researcher example"
  [ testCase "concrete spec installs only synchronous Web domain handlers" $ do
      assertEqual "delegable effects" ["Web"]
        (toolEffectNames (Proxy @Researcher.ResearchEffects))
      case prepareAgentSpec Researcher.researcherSpec of
        Left failure -> assertFailure (show failure)
        Right prepared -> do
          let metadata = preparedAgentSpecMetadata prepared
          case field "tools" metadata of
            Array tools -> do
              assertEqual "domain tool names" [String "search", String "open"]
                (map (field "name") tools)
              mapM_ (\tool -> do
                assertEqual "sync scheduling" (String "before_next_inference") (field "schedule" tool)
                assertEqual "ordinary handlers" (String "resident_handler") (field "implementation" tool)
                assertEqual "object input" (String "object") (field "type" (field "inputSchema" tool))) tools
            other -> assertFailure (show other)
          assertEqual "no startup or hooks" (Array []) (field "slots" metadata)
  , testCase "successive report schemas decode their own source-backed records" $ do
      let citation = object ["sourceUrl" .= ("https://example.org/moss" :: Text), "sourceReference" .= ("page-reference" :: Text)]
          report = object ["question" .= ("What grows?" :: Text), "summary" .= ("moss λ" :: Text), "sources" .= [citation]]
          followUp = object ["answer" .= ("It is green." :: Text), "citations" .= (["https://example.org/moss"] :: [Text])]
      assertEqual "first typed report" (Success (Researcher.Report "What grows?" "moss λ"
        [Researcher.Citation "https://example.org/moss" "page-reference"])) (fromJSON report)
      assertEqual "second typed report" (Success (Researcher.FollowUp "It is green." ["https://example.org/moss"]))
        (fromJSON followUp)
      assertBool "first response cannot decode as the second result type"
        (case fromJSON report :: Result Researcher.FollowUp of Error _ -> True; Success _ -> False)
      assertEqual "first schema fields" ["question", "sources", "summary"]
        (keys (field "properties" (jsonSchema (Proxy @Researcher.Report))))
      assertEqual "second schema fields" ["answer", "citations"]
        (keys (field "properties" (jsonSchema (Proxy @Researcher.FollowUp))))
  , testCase "explicit and reply displays preserve complete report JSON" $ do
      let report = Researcher.Report "What grows?" "moss λ"
            [Researcher.Citation "https://example.org/moss" "page-reference"]
          followUp = Researcher.FollowUp "It is green." ["https://example.org/moss"]
      assertEqual "explicit report" (presentJson report, False) (displayWith 10000 report)
      assertEqual "reply report" (presentJson report, False) (workbenchDisplay report)
      assertEqual "explicit follow-up" (presentJson followUp, False) (displayWith 10000 followUp)
      assertEqual "reply follow-up" (presentJson followUp, False) (workbenchDisplay followUp)
  , testCase "domain handlers preserve Web request arguments and typed failures" $ do
      assertEqual "search domain arguments" (Right (Array []))
        (run (interpret webHost (Researcher.searchHandler (Researcher.SearchInput "moss" 3))))
      assertEqual "open domain failure" (Left (WebTransport "controlled extraction failure"))
        (run (interpret webHost (Researcher.openHandler (Researcher.OpenInput "https://example.org/moss"))))
  ]
  where
    keys (Object values) = Map.keys values
    keys other = error (show other)

webHost :: Web value -> Eff '[] value
webHost (WebSearch query count included excluded)
  | query == "moss" && count == 3 && null included && null excluded = pure (Right (Array []))
webHost (WebExtract target)
  | target == "https://example.org/moss" = pure (Left (WebTransport "controlled extraction failure"))
webHost _ = error "the example invoked a different Web operation or changed its arguments"
