{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | One bounded model turn, with tools that run in the caller's effect row.
-- Define a reusable turn with 'textTurn' or 'typedTurn', then supply its input
-- to 'invokeModel'. There is no conversation to manage. Every invocation in
-- the same admitted cell shares a host budget, including invocations made by
-- callbacks. Tool callbacks run sequentially; the supplied 'AgentSpec' owns
-- its after-tool hook. Ambient actor tools and hooks are not inherited.
module Tidepool.Model
  ( ModelCall
  , ModelTurn
  , textTurn
  , typedTurn
  , withModel
  , withEffort
  , withLimits
  , ModelEffort (..)
  , ModelLimits (..)
  , defaultLimits
  , invokeModel
  , ModelResult (..)
  , ModelFailure (..)
  , ModelBoundaryError (..)
  , ModelLimit (..)
  , ModelReceipt (..)
  , ModelUsage (..)
  ) where

import Prelude
import Control.Monad.Freer (Eff, Member, send)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ModelCall (..), ModelBoundaryError (..))

-- | Requested effort is constrained by the host's admitted model policy.
data ModelEffort = LowEffort | MediumEffort | HighEffort
  deriving (Eq, Show)

-- | Optional narrower limits. Missing fields retain the cell's limits;
-- negative values are refused before any provider request. Reported token
-- limits are a runaway breaker, not a precharged monetary ceiling.
data ModelLimits = ModelLimits
  { requestLimit :: Maybe Int
  , toolLimit :: Maybe Int
  , reportedTokenLimit :: Maybe Int
  , durationSeconds :: Maybe Int
  } deriving (Eq, Show)

defaultLimits :: ModelLimits
defaultLimits = ModelLimits Nothing Nothing Nothing Nothing

data ResultFormat a where
  TextResult :: ResultFormat Text
  JsonResult :: (FromJSON a, JsonSchema a) => Proxy a -> ResultFormat a

data ModelTurn tools effects a = ModelTurn
  { turnSpec :: AgentSpec tools effects
  , turnInstructions :: Text
  , turnFormat :: ResultFormat a
  , turnModel :: Maybe Text
  , turnEffort :: Maybe ModelEffort
  , turnLimits :: ModelLimits
  }

textTurn :: AgentSpec tools effects -> Text -> ModelTurn tools effects Text
textTurn spec instructions = ModelTurn spec instructions TextResult Nothing Nothing defaultLimits

typedTurn :: forall a tools effects. (FromJSON a, JsonSchema a) => AgentSpec tools effects -> Text -> ModelTurn tools effects a
typedTurn spec instructions = ModelTurn spec instructions (JsonResult (Proxy @a)) Nothing Nothing defaultLimits

withModel :: Text -> ModelTurn tools effects a -> ModelTurn tools effects a
withModel model turn = turn { turnModel = Just model }
withEffort :: ModelEffort -> ModelTurn tools effects a -> ModelTurn tools effects a
withEffort effort turn = turn { turnEffort = Just effort }
withLimits :: ModelLimits -> ModelTurn tools effects a -> ModelTurn tools effects a
withLimits limits turn = turn { turnLimits = limits }

data ModelLimit = ProviderRequests | ToolAttempts | ReportedTokens | Deadline
  deriving (Eq, Show)
data ModelFailure
  = InvalidModelSpec Text
  | ModelBoundary ModelBoundaryError
  | InvalidModelResponse Text
  | ModelBudgetExceeded ModelLimit
  | ModelFailed Text
  | ModelCancelled
  | ModelCleanupFailed ModelFailure ModelBoundaryError
  deriving (Show)

data ModelUsage = ModelUsage
  { providerRequests :: Int
  , toolAttempts :: Int
  , reportedTokens :: Int
  , requestsWithoutUsage :: Int
  } deriving (Eq, Show)

-- | References identify evidence retained by the host. Full transcripts are
-- not copied into every notebook result. Usage is per invocation; nested
-- invocations have their own receipts and share the cell's aggregate limit.
data ModelReceipt = ModelReceipt
  { invocationIdentity :: Text
  , parentCellIdentity :: Text
  , requestIdentities :: [Text]
  , invocationUsage :: ModelUsage
  } deriving (Eq, Show)
data ModelResult a = ModelResult
  { modelOutcome :: Either ModelFailure a
  , modelReceipt :: Maybe ModelReceipt
  } deriving (Show)

-- | Carry one turn through its callbacks and return at its final answer or
-- typed failure. Budget exhaustion stops new model work; an already-running
-- Haskell callback finishes cooperatively. Ordinary Haskell cleanup can run
-- after this function returns. Parent-cell cancellation follows the engine's
-- cancellation contract and cannot be caught by this wrapper.
-- Callback and after-tool programs use the ordinary row, even inside a
-- synchronous notebook. Raise a base-row invocation into the enclosing row.
invokeModel :: (Member ModelCall effects, HasAgentApi tools (Eff effects), AsyncEffects effects)
            => ModelTurn tools effects a -> Text -> Eff effects (ModelResult a)
invokeModel turn input = case compileTools (specTools (turnSpec turn)) of
  Left err -> pure (failed (InvalidModelSpec (renderToolCompileError err)))
  Right compiled -> do
    initial <- send (ModelStartWith (request compiled))
    drive compiled Nothing initial
  where
    request compiled = object
      [ "instructions" .= turnInstructions turn
      , "input" .= input
      , "model" .= turnModel turn
      , "effort" .= fmap effortText (turnEffort turn)
      , "limits" .= limitsValue (turnLimits turn)
      , "tools" .= declarationsToJson (declarations compiled)
      , "result_schema" .= resultSchema (turnFormat turn)
      , "after_tool" .= maybe False (const True) (afterTool (turnSpec turn))
      ]
    stop Nothing failure = pure (failed failure)
    stop (Just token) failure = do
      closed <- send (ModelCloseWith token)
      pure (failed (either (ModelCleanupFailed failure) (const failure) closed))
    drive _ active (Left err) = stop active (ModelBoundary err)
    drive compiled active (Right value) = case parseStep value of
      Error err -> stop active (InvalidModelResponse (T.pack err))
      Success (Finished receipt outcome) ->
        pure (ModelResult (decodeOutcome (turnFormat turn) outcome) (Just receipt))
      Success (Callback token callId name args) -> do
        answer <- dispatch compiled name args
        next <- send (ModelResumeWith token callId (toolDispatchReply answer))
        drive compiled (Just token) next
      Success (Hook token operation name args handle ordinal semantic output) -> do
        annotation <- case afterTool (turnSpec turn) of
          Nothing -> pure NoAnnotation
          Just hook -> hook (ToolCall name args) (ToolResult name handle ordinal semantic output)
        next <- send (ModelAnnotateWith token operation (annotationToJson annotation))
        drive compiled (Just token) next

failed :: ModelFailure -> ModelResult a
failed failure = ModelResult (Left failure) Nothing

effortText :: ModelEffort -> Text
effortText LowEffort = "low"
effortText MediumEffort = "medium"
effortText HighEffort = "high"
limitsValue :: ModelLimits -> Value
limitsValue limits = object
  [ "requests" .= requestLimit limits, "tools" .= toolLimit limits
  , "reported_tokens" .= reportedTokenLimit limits, "seconds" .= durationSeconds limits ]
resultSchema :: ResultFormat a -> Maybe Value
resultSchema TextResult = Nothing
resultSchema (JsonResult proxy) = Just (jsonSchema proxy)

data Step
  = Callback Text Text Text Value
  | Hook Text Text Text Value Text Int Value Text
  | Finished ModelReceipt Value

parseStep :: Value -> Result Step
parseStep = withObject "model step" $ \o -> do
  kind <- o .: "kind"
  case (kind :: Text) of
    "callback" -> Callback <$> o .: "invocation" <*> o .: "call_id" <*> o .: "name" <*> o .: "arguments"
    "hook" -> Hook <$> o .: "invocation" <*> o .: "operation" <*> o .: "name" <*> o .: "arguments" <*> o .: "handle" <*> o .: "ordinal" <*> ((o .:? "value") .!= Null) <*> o .: "output"
    "finished" -> do
      receipt <- o .: "receipt"
      withObject "model receipt" (\r -> Finished <$> parseReceipt receipt <*> r .: "outcome") receipt
    _ -> Error "unknown model step"

parseReceipt :: Value -> Result ModelReceipt
parseReceipt = withObject "model receipt" $ \o -> do
  counts <- o .: "counts"
  usage <- withObject "model usage" (\c -> ModelUsage <$> c .: "requests" <*> c .: "tools" <*> c .: "reported_tokens" <*> c .: "unknown_usage_requests") counts
  ModelReceipt <$> o .: "invocation_id" <*> o .: "parent_cell" <*> o .: "requests" <*> pure usage

decodeOutcome :: ResultFormat a -> Value -> Either ModelFailure a
decodeOutcome format value = case withObject "model outcome" decode value of
  Error err -> Left (InvalidModelResponse (T.pack err))
  Success result -> result
  where
    decode o = do
      kind <- o .: "kind"
      case (kind :: Text) of
        "text" -> case format of
          TextResult -> Right <$> o .: "value"
          JsonResult _ -> Error "text returned for typed model turn"
        "typed" -> case format of
          JsonResult _ -> Right <$> o .: "value"
          TextResult -> Error "typed value returned for text model turn"
        "failed" -> Left . ModelFailed <$> o .: "value"
        "cancelled" -> pure (Left ModelCancelled)
        "exhausted" -> do
          reason <- o .: "value"
          case (reason :: Text) of
            "requests" -> pure (Left (ModelBudgetExceeded ProviderRequests))
            "tools" -> pure (Left (ModelBudgetExceeded ToolAttempts))
            "reported_tokens" -> pure (Left (ModelBudgetExceeded ReportedTokens))
            "deadline" -> pure (Left (ModelBudgetExceeded Deadline))
            _ -> Error "unknown model budget dimension"
        _ -> Error "unknown model outcome"
