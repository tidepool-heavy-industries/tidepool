{-# LANGUAGE OverloadedStrings #-}
module Main where
import Prelude
import Data.Text (Text)
import Tidepool.Aeson
import Tidepool.Internal.ModelControl
import Tidepool.Effects.Core (Backend (..), Model (..), OpenAIModel (..), Reasoning (..), DeepSeekReasoning (..), OpenAIReasoning (..), ArgonEffort (..), SpecValidationReceipt (..), SpecValidationError (..), SpecProvenance (..), SpecProviderValidation (..), JevCallError (..))

assert :: Bool -> String -> IO ()
assert True _ = pure ()
assert False message = fail message
roundtrip :: (Eq a, Show a, ToJSON a, FromJSON a) => a -> IO ()
roundtrip value = assert (fromJSON (toJSON value) == Success value) (show value)
main :: IO ()
main = do
  let semantic = object ["answer" .= (42 :: Int)]
      counts = ModelUsageEnvelope 1 1 8 0
      observation = ModelRequestObservationEnvelope "request" "deep_seek" "actual-provider-model"
        (Just 7) (Just 0) Nothing Nothing 12
      receipt = ModelReceiptEnvelope "invocation" "cell" ["request"] [observation] 23 counts counts (ModelTypedOutcome semantic)
      hook = ModelHook "invocation" "operation" "tool" Null "retained" 3 semantic "output"
  roundtrip observation
  assert (toJSON observation == object ["request_id" .= ("request" :: Text), "provider" .= ("deep_seek" :: Text), "model" .= ("actual-provider-model" :: Text), "input_tokens" .= (Just 7 :: Maybe Int), "cached_tokens" .= (Just 0 :: Maybe Int), "output_tokens" .= (Nothing :: Maybe Int), "reasoning_tokens" .= (Nothing :: Maybe Int), "duration_ms" .= (12 :: Int)]) "request observation lost missing versus zero or identity"
  mapM_ roundtrip [ModelCallback "invocation" "operation" "tool" semantic, hook, ModelFinished receipt]
  mapM_ roundtrip [ModelNoAnnotation,ModelAbstained "reason",ModelAnnotated "text",ModelPruned "handle" "short"]
  let chosen = ModelSelectionEnvelope "catalog-hash" "argon" (ArgonReasoning EffortMedium)
  roundtrip chosen
  assert (toJSON chosen == object
    ["catalog_identity" .= ("catalog-hash" :: Text), "key" .= ("argon" :: Text),
     "reasoning" .= object ["tag" .= ("ArgonReasoning" :: Text),
       "contents" .= ("EffortMedium" :: Text)]])
    "catalog selection lost exact identity, key or typed reasoning"
  roundtrip (ModelRequestEnvelope "instructions" "input"
    (ModelRequestLimits Nothing Nothing Nothing Nothing) (toJSON ([] :: [Value])) Nothing True chosen)
  roundtrip (ModelGrantRequest ModelCall "catalog-hash" "argon" [ArgonReasoning EffortMedium])
  assert (toJSON (ModelGrantRequest ForkAgent "catalog-hash" "argon" [ArgonReasoning EffortHigh]) == object
    ["operation" .= ("fork_agent" :: Text), "catalog_identity" .= ("catalog-hash" :: Text),
     "key" .= ("argon" :: Text), "efforts" .= [ArgonReasoning EffortHigh]])
    "child model grant lost operation, catalog key or allowed efforts"
  mapM_ roundtrip [DeepSeekReasoning DeepSeekHigh, OpenAIReasoning OpenAIMedium, ArgonReasoning EffortHigh]
  assert (toJSON GeminiArgon == object ["tag" .= ("GeminiArgon" :: Text)]) "Argon model selector shape changed"
  assert (toJSON (CustomModel DeepSeek "registered-custom") == object
    ["tag" .= ("CustomModel" :: Text), "contents" .= toJSON (DeepSeek, "registered-custom" :: Text)])
    "custom model lost its explicit backend"
  mapM_ roundtrip
    [JevUnconfigured, JevCallCap, JevTransport "断線", JevTimeout,
     JevHttp 429 "後で", JevCircuitOpen 503 987, JevClientSetup "setup",
     JevBodyLimit, JevMalformed "malformed"]
  assert (fromJSON (object ["tag" .= ("JevHttp" :: String), "contents" .= [Number (scientific 429 0), String "後で"]]) == Success (JevHttp 429 "後で"))
    "independent generated payload shape did not decode"
  mapM_ (\malformed -> case (fromJSON malformed :: Result JevCallError) of
      Error _ -> pure ()
      Success _ -> fail "malformed typed Jev failure was accepted")
    [object ["tag" .= ("UnknownCause" :: String)],
     object ["tag" .= ("JevHttp" :: String)],
     object ["tag" .= ("JevHttp" :: String), "contents" .= [String "429", Null]],
     object ["tag" .= ("JevCircuitOpen" :: String), "contents" .= [Number (scientific 503 0)]]]
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

  let provenance = SpecProvenanceUnavailable "arbitrary concrete value"
      checked = SpecValidationReceipt ["search", "open"] ["Web"] provenance SpecProviderChecked
      unchecked = SpecValidationReceipt [] [] provenance SpecProviderNotChecked
  assert (toJSON (SpecInputSchemaInvalid "object input required") == object ["tag" .= ("SpecInputSchemaInvalid" :: String), "contents" .= ("object input required" :: String)]) "input refusal category lost"
  assert (toJSON (SpecProviderProjectionInvalid "provider rejected schema") == object ["tag" .= ("SpecProviderProjectionInvalid" :: String), "contents" .= ("provider rejected schema" :: String)]) "provider refusal category lost"
  assert (toJSON SpecProviderChecked == String "SpecProviderChecked" && toJSON SpecProviderNotChecked == String "SpecProviderNotChecked") "provider coverage tags changed"
  assert (toJSON checked == object ["specValidatedEndpoints" .= (["search", "open"] :: [String]), "specRequestedEffects" .= (["Web"] :: [String]), "specProvenance" .= object ["specProvenanceReason" .= ("arbitrary concrete value" :: String)], "specProviderValidation" .= SpecProviderChecked]) "receipt lost exact facts"
  assert (toJSON checked /= toJSON unchecked) "provider coverage or endpoint facts lost"
  case toJSON checked of
    String _ -> fail "spec receipt collapsed to rendered prose"
    _ -> pure ()
