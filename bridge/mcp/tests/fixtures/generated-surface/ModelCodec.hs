{-# LANGUAGE OverloadedStrings #-}
module Main where
import Prelude
import Tidepool.Aeson
import Tidepool.Internal.ModelControl

assert :: Bool -> String -> IO ()
assert True _ = pure ()
assert False message = fail message
roundtrip :: (Eq a, Show a, ToJSON a, FromJSON a) => a -> IO ()
roundtrip value = assert (fromJSON (toJSON value) == Success value) (show value)
main :: IO ()
main = do
  let semantic = object ["answer" .= (42 :: Int)]
      counts = ModelUsageEnvelope 1 1 8 0
      receipt = ModelReceiptEnvelope "invocation" "cell" ["request"] counts counts (ModelTypedOutcome semantic)
      hook = ModelHook "invocation" "operation" "tool" Null "retained" 3 semantic "output"
  mapM_ roundtrip [ModelCallback "invocation" "operation" "tool" semantic, hook, ModelFinished receipt]
  mapM_ roundtrip [ModelNoAnnotation,ModelAbstained "reason",ModelAnnotated "text",ModelPruned "handle" "short"]
  roundtrip (ModelRequestEnvelope "instructions" "input" Nothing (Just ModelLowEffort)
    (ModelRequestLimits Nothing Nothing Nothing Nothing) (toJSON ([] :: [Value])) Nothing True)
  case fromJSON (toJSON hook) of
    Success (ModelHook _ _ _ _ _ _ value _) -> assert (value == semantic) "callback value changed"
    _ -> fail "valid hook control failed"
  let missing = object ["kind" .= ("hook" :: String),"invocation" .= ("invocation" :: String),"operation" .= ("operation" :: String),"name" .= ("tool" :: String),"arguments" .= Null,"handle" .= ("retained" :: String),"ordinal" .= (3 :: Int),"output" .= ("output" :: String)]
  case (fromJSON missing :: Result ModelControlStep) of
    Error _ -> pure ()
    Success _ -> fail "missing callback value was accepted"
  case (fromJSON (object ["kind" .= ("unknown" :: String)]) :: Result ModelControlStep) of
    Error _ -> pure ()
    Success _ -> fail "unknown control variant was accepted"
