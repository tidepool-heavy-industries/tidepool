{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module JevRecordingTest (jevRecordingTests) where

import Control.Monad.Freer (Eff, interpret, run)
import qualified Data.Map.Strict as Map
import Data.Text (Text)
import qualified Data.Text as Text
import Jev.Core.Query (Occurrence (..), Recording (..))
import qualified Jev.Operators as J
import Jev.Recording
import Test.Tasty.HUnit (assertEqual, assertFailure)
import Test.Tasty.QuickCheck
import Tidepool.Aeson
import Tidepool.Effects.Core (Jev (..), JevCallError (..))
import Tidepool.Test.Runner (TestTree, testCase, testGroup)

jevRecordingTests :: TestTree
jevRecordingTests = testGroup "jev-recording"
  [ testProperty "structural histories preserve exact JSON and every typed host cause" structuralHistory
  , testCase "empty and failed evaluation prefixes remain observations" failurePrefix
  , testCase "unknown versions and malformed histories refuse atomically" malformedArchives
  , testCase "independent v1 archive decodes Unicode, occurrence order and exact numbers" independentArchive
  ]

hostFailures :: [JevCallError]
hostFailures =
  [ JevUnconfigured, JevCallCap, JevTransport "断線\ntransport", JevTimeout
  , JevHttp 503 "échec", JevCircuitOpen 503 987, JevClientSetup "setup"
  , JevBodyLimit, JevMalformed "malformed"
  ]

-- The generator uses the owning Value constructors directly, rather than
-- Jev's Double-valued JSON view or an alternative text codec.
jsonValue :: Gen Value
jsonValue = sized go
  where
    leaf = oneof
      [ pure Null, Bool <$> arbitrary, String . Text.pack <$> arbitrary
      , Number <$> (scientific <$> arbitrary <*> choose (-500, 500))
      ]
    go size | size <= 0 = leaf
    go size = frequency
      [ (5, leaf)
      , (1, Array <$> resize (min 5 size) (listOf (go (size `div` 3))))
      , (1, object <$> resize (min 5 size)
          (listOf ((,) <$> elements ["ordinary", "雪", "", "escaped\nkey"] <*> go (size `div` 3))))
      ]

shrinkValue :: Value -> [Value]
shrinkValue Null = []
shrinkValue (Array values) = Null : map Array (shrinkList shrinkValue values)
shrinkValue (Object values) = Null : map object (shrinkList shrinkPair (Map.toList values))
  where shrinkPair (key, value) = [(key, smaller) | smaller <- shrinkValue value]
shrinkValue _ = [Null]

structuralHistory :: Property
structuralHistory = forAllShrink (resize 12 (listOf jsonValue)) (shrinkList shrinkValue) $ \values ->
  let occurrences n = [Occurrence n ["outer", "内側"] ["question", "質問"], Occurrence (n + 1) [] []]
      successes = [Recording value (occurrences n) (Right value) | (n, value) <- zip [0..] values]
      failures = [Recording (object ["input" .= values]) (occurrences n) (Left failure)
        | (n, failure) <- zip [0..] hostFailures]
      history = recordingArchive (successes ++ failures)
  in classify (null values) "empty success prefix" $
     classify (length values > 2) "multiple ordered responses" $
     archiveFromValue (archiveValue history) === Right history

failurePrefix :: IO ()
failurePrefix = do
  let query = traverse (\n -> J.question (J.state (#input J.:= (n :: Int))) #ready (J.noul "Ready?")) [1,2]
      refuse :: Jev a -> Eff '[] a
      refuse (JevAskWith _) = pure (Left (JevCircuitOpen 503 987))
      observed = run (interpret refuse (J.runQuery query))
      archive = recordingArchive (J.recordings observed)
  assertEqual "only attempted group archived" 1 (length (archiveRecords archive))
  assertEqual "typed failed prefix survives" (Right archive) (archiveFromValue (archiveValue archive))
  case J.prepareQuery J.jevLatest query of
    Left failure -> assertFailure (show failure)
    Right ready -> case J.result (J.replayQuery ready (archiveRecords archive)) of
      Left actual -> assertEqual "existing replay retains host refusal"
        (J.CallFailed (J.Transport (JevCircuitOpen 503 987))) actual
      Right _ -> assertFailure "failed recording unexpectedly replayed successfully"
  let empty = recordingArchive []
  assertEqual "empty archive" (Right empty) (archiveFromValue (archiveValue empty))
  let preparationFailure = run (interpret refuse (J.runQuery
        (J.question (J.rawState Null) #ready
          (J.choice "Choose?" (J.many #none fst snd ([] :: [(Text, Text)]))))))
  assertEqual "preparation refusal has no recordings" [] (J.recordings preparationFailure)
  case J.result preparationFailure of
    Left (J.CallFailed (J.Prepare _)) -> pure ()
    _ -> assertFailure "empty offer must fail preparation before transport"

malformedArchives :: IO ()
malformedArchives = do
  assertEqual "unknown integral version is distinguished"
    (Left (UnsupportedVersion 2)) (archiveFromValue (object ["version" .= (2 :: Integer), "records" .= ([] :: [Value])]))
  let record = object ["request" .= Null, "occurrences" .= ([] :: [Value]), "outcome" .= (Right Null :: Either JevCallError Value)]
      envelope records = object ["version" .= (1 :: Integer), "records" .= records]
      malformed =
        [ Null
        , object ["request" .= Null, "occurrences" .= ([] :: [Value])]
        , object ["request" .= Null, "occurrences" .= [object ["index" .= (1.5 :: Double), "scopes" .= ([] :: [Text]), "questions" .= ([] :: [Text])]], "outcome" .= (Right Null :: Either JevCallError Value)]
        , object ["request" .= Null, "occurrences" .= [object ["index" .= (2 ^ (128 :: Int) :: Integer), "scopes" .= ([] :: [Text]), "questions" .= ([] :: [Text])]], "outcome" .= (Right Null :: Either JevCallError Value)]
        , object ["request" .= Null, "occurrences" .= ([] :: [Value]), "outcome" .= object ["Left" .= object ["tag" .= ("UnknownCause" :: Text)]]]
        , object ["request" .= Null, "occurrences" .= ([] :: [Value]), "outcome" .= object ["Left" .= object ["tag" .= ("JevHttp" :: Text), "contents" .= [String "bad", Null]]]]
        , object ["request" .= Null, "occurrences" .= ([] :: [Value]), "outcome" .= object ["Left" .= object ["tag" .= ("JevTimeout" :: Text)], "Right" .= Null]]
        ]
  mapM_ (\bad -> case archiveFromValue (envelope [record, bad]) of
    Left (MalformedRecord 1 _) -> pure ()
    actual -> assertFailure (show actual)) malformed
  mapM_ (\bad -> case archiveFromValue bad of
    Left (InvalidArchive _) -> pure ()
    actual -> assertFailure (show actual))
    [Null, object [], object ["version" .= (1.5 :: Double), "records" .= ([] :: [Value])],
     object ["version" .= (1 :: Int), "records" .= Null]]

independentArchive :: IO ()
independentArchive = do
  let exact = Number (scientific 123456789012345678901234567890123456789 (-97))
      expected = Recording (object ["雪" .= exact])
        [Occurrence 7 ["外", "内"] ["q"], Occurrence 3 [] ["q2"]]
        (Left (JevHttp 429 "後で"))
      literal = object
        [ "version" .= (1 :: Int)
        , "records" .= [object
            [ "request" .= object ["雪" .= exact]
            , "occurrences" .= [object ["index" .= (7 :: Int), "scopes" .= ["外", "内" :: Text], "questions" .= ["q" :: Text]], object ["index" .= (3 :: Int), "scopes" .= ([] :: [Text]), "questions" .= ["q2" :: Text]]]
            , "outcome" .= object ["Left" .= object ["tag" .= ("JevHttp" :: Text), "contents" .= [Number (scientific 429 0), String "後で"]]]
            ]]
        ]
  assertEqual "v1 shape independently constructed" (Right (recordingArchive [expected])) (archiveFromValue literal)
