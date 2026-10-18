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
  , ForkAgent
  , SpawnAgent
  , ModelTurn
  , textTurn
  , typedTurn
  , withModel
  , withLimits
  , module Tidepool.Model.Selection
  , ModelLimits (..)
  , defaultLimits
  , invokeModel
  , ModelResult (..)
  , ModelFailure (..)
  , ModelBoundaryError (..)
  , ModelLimit (..)
  , ModelReceipt (..)
  , ModelUsage (..)
  , ModelRequestObservation (..)
  ) where

import Prelude
import Control.Monad.Freer (Eff, Member, send)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson
import Tidepool.Model.Selection
import Tidepool.Internal.ModelControl hiding (ModelOperation (..))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ModelCall (..), ForkAgent, SpawnAgent, ModelBoundaryError (..), KnownFamily (familyWitness))

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

data ModelTurn modelFamily tools effects a = ModelTurn
  { turnSpec :: AgentSpec tools effects
  , turnInstructions :: Text
  , turnFormat :: ResultFormat a
  , turnModel :: Model modelFamily
  , turnLimits :: ModelLimits
  }

textTurn :: Model modelFamily -> AgentSpec tools effects -> Text -> ModelTurn modelFamily tools effects Text
textTurn model spec instructions = ModelTurn spec instructions TextResult model defaultLimits

typedTurn :: forall a modelFamily tools effects. (FromJSON a, JsonSchema a) => Model modelFamily -> AgentSpec tools effects -> Text -> ModelTurn modelFamily tools effects a
typedTurn model spec instructions = ModelTurn spec instructions (JsonResult (Proxy @a)) model defaultLimits

-- | Select another exact model and effort in the same provider family.
withModel :: Model modelFamily -> ModelTurn modelFamily tools effects a -> ModelTurn modelFamily tools effects a
withModel model turn = turn { turnModel = model }
withLimits :: ModelLimits -> ModelTurn modelFamily tools effects a -> ModelTurn modelFamily tools effects a
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

-- | Per-request counters retain missing reports separately from explicit zero.
-- The request identity resolves retained host transport and raw usage evidence.
-- Output includes reasoning tokens; do not add reasoning to output.
-- Duration covers the transport await, including stream consumption.
data ModelRequestObservation = ModelRequestObservation
  { observedRequestIdentity :: Text
  , observedProvider :: Text
  , observedModel :: Text
  , observedInputTokens :: Maybe Int
  , observedCachedTokens :: Maybe Int
  , observedOutputTokens :: Maybe Int
  , observedReasoningTokens :: Maybe Int
  , requestDurationMs :: Int
  } deriving (Eq, Show)

-- | References identify evidence retained by the host. Full transcripts are
-- not copied into every result. Usage is per invocation; nested invocations
-- have their own receipts and share the owning execution's aggregate limit.
data ModelReceipt = ModelReceipt
  { invocationIdentity :: Text
  , parentCellIdentity :: Text
    -- ^ Owning execution identity: the notebook cell for notebook calls, or
    -- the retained actor-scope execution for native Record RPCs. This is not
    -- necessarily a notebook cell reference in the Store.
  , requestIdentities :: [Text]
  , requestObservations :: [ModelRequestObservation]
  , invocationDurationMs :: Int
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
invokeModel :: forall modelFamily tools effects a. (KnownFamily modelFamily, Member (ModelCall modelFamily) effects, HasAgentApi tools (Eff effects), AsyncEffects effects)
            => ModelTurn modelFamily tools effects a -> Text -> Eff effects (ModelResult a)
invokeModel turn input = case compileTools (specTools (turnSpec turn)) of
  Left err -> pure (failed (InvalidModelSpec (renderToolCompileError err)))
  Right compiled -> do
    initial <- send @(ModelCall modelFamily) (ModelStartWith (familyWitness @modelFamily) (request compiled))
    drive compiled Nothing initial
  where
    request compiled = toJSON (ModelRequestEnvelope
      (turnInstructions turn) input
      (requestLimits (turnLimits turn)) (declarationsToJson (declarations compiled))
      (resultSchema (turnFormat turn)) (maybe False (const True) (afterTool (turnSpec turn))) (modelSelection (turnModel turn)))
    stop Nothing failure = pure (failed failure)
    stop (Just token) failure = do
      closed <- send @(ModelCall modelFamily) (ModelCloseWith (familyWitness @modelFamily) token)
      pure (failed (either (ModelCleanupFailed failure) (const failure) closed))
    drive _ active (Left err) = stop active (ModelBoundary err)
    drive compiled active (Right value) = case fromJSON value of
      Error err -> stop active (InvalidModelResponse (T.pack err))
      Success (ModelFinished receipt) ->
        pure (ModelResult (decodeOutcome (turnFormat turn) (receiptOutcome receipt)) (Just (publicReceipt receipt)))
      Success (ModelCallback token callId name args) -> do
        answer <- dispatch compiled name args
        next <- send @(ModelCall modelFamily) (ModelResumeWith (familyWitness @modelFamily) token callId (toolDispatchReply answer))
        drive compiled (Just token) next
      Success (ModelHook token operation name args handle ordinal semantic output) -> do
        annotation <- case afterTool (turnSpec turn) of
          Nothing -> pure NoAnnotation
          Just hook -> hook (ToolCall name args) (ToolResult name handle ordinal semantic output)
        next <- send @(ModelCall modelFamily) (ModelAnnotateWith (familyWitness @modelFamily) token operation (toJSON (controlAnnotation annotation)))
        drive compiled (Just token) next

failed :: ModelFailure -> ModelResult a
failed failure = ModelResult (Left failure) Nothing

requestLimits :: ModelLimits -> ModelRequestLimits
requestLimits limits = ModelRequestLimits (requestLimit limits) (toolLimit limits)
  (reportedTokenLimit limits) (durationSeconds limits)

resultSchema :: ResultFormat a -> Maybe Value
resultSchema TextResult = Nothing
resultSchema (JsonResult proxy) = Just (jsonSchema proxy)

publicReceipt :: ModelReceiptEnvelope -> ModelReceipt
publicReceipt (ModelReceiptEnvelope identity cell requests observations duration (ModelUsageEnvelope rq tools tokens unknown) _ _) =
  ModelReceipt identity cell requests (map publicObservation observations) duration (ModelUsage rq tools tokens unknown)

publicObservation :: ModelRequestObservationEnvelope -> ModelRequestObservation
publicObservation (ModelRequestObservationEnvelope request provider model input cached output reasoning duration) =
  ModelRequestObservation request provider model input cached output reasoning duration

receiptOutcome :: ModelReceiptEnvelope -> ModelOutcomeEnvelope
receiptOutcome (ModelReceiptEnvelope _ _ _ _ _ _ _ outcome) = outcome

controlAnnotation :: Annotation -> ModelAnnotationEnvelope
controlAnnotation NoAnnotation = ModelNoAnnotation
controlAnnotation (Abstained reason) = ModelAbstained reason
controlAnnotation (Annotated text) = ModelAnnotated text
controlAnnotation (Pruned text handle) = ModelPruned handle text

decodeOutcome :: ResultFormat a -> ModelOutcomeEnvelope -> Either ModelFailure a
decodeOutcome format outcome = case outcome of
  ModelTextOutcome value -> case format of
    TextResult -> Right value
    JsonResult _ -> Left (InvalidModelResponse "text returned for typed model turn")
  ModelTypedOutcome value -> case format of
    JsonResult _ -> case fromJSON value of
      Success answer -> Right answer
      Error err -> Left (InvalidModelResponse (T.pack err))
    TextResult -> Left (InvalidModelResponse "typed value returned for text model turn")
  ModelFailedOutcome detail -> Left (ModelFailed detail)
  ModelCancelledOutcome -> Left ModelCancelled
  ModelExhaustedOutcome reason -> Left $ case reason of
    ModelRequestsLimit -> ModelBudgetExceeded ProviderRequests
    ModelToolsLimit -> ModelBudgetExceeded ToolAttempts
    ModelTokensLimit -> ModelBudgetExceeded ReportedTokens
    ModelDeadlineLimit -> ModelBudgetExceeded Deadline
