{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | A bounded decision over explicit input. It has no tools or ambient context.
module Examples.ModelCallWorkflow where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Agent.Contract (AsyncEffects, NoTools (..), SyncEffects, defaultSpec, raiseSync, specTools)
import qualified Tidepool.Model as Model

noToolsTurn :: Model.Model -> Model.ModelTurn NoTools effects Text
noToolsTurn model = Model.withModel model
  (Model.withLimits (Model.defaultLimits
    { Model.requestLimit = Just 1, Model.toolLimit = Just 0 })
    (Model.textTurn (defaultSpec { specTools = NoTools })
      "Read only the supplied input. Return one concise conclusion and identify missing evidence. Do not claim to have inspected anything outside this input."))

luna :: (Member Model.ModelCall effects, AsyncEffects effects)
  => Text -> Eff effects (Model.ModelResult Text)
luna = Model.invokeModel (noToolsTurn Model.Luna)

sol :: (Member Model.ModelCall effects, AsyncEffects effects)
  => Text -> Eff effects (Model.ModelResult Text)
sol = Model.invokeModel (noToolsTurn Model.Sol)

-- Reuse an ordinary base-row program inside a synchronous notebook. Callbacks
-- would still use that base row; this particular turn offers no callbacks.
synchronousLuna :: Text -> Eff (SyncEffects '[Model.ModelCall]) (Model.ModelResult Text)
synchronousLuna input = raiseSync (luna input :: Eff '[Model.ModelCall] (Model.ModelResult Text))

-- Keep the original ModelResult bound: this presentation includes the receipt
-- on both success and failure, without converting a typed failure into success.
report :: Model.ModelResult Text
  -> (Either Text Text, Maybe ([(Text, Text)], [Text], [(Text, Int)]))
report result =
  (either (Left . Text.pack . show) Right (Model.modelOutcome result),
    fmap receiptMetadata (Model.modelReceipt result))
  where
    receiptMetadata receipt =
      let usage = Model.invocationUsage receipt
      in ( [("invocation", Model.invocationIdentity receipt),
              ("parent_cell", Model.parentCellIdentity receipt)]
         , Model.requestIdentities receipt
         , [("provider_requests", Model.providerRequests usage),
              ("tool_attempts", Model.toolAttempts usage),
              ("reported_tokens", Model.reportedTokens usage),
              ("requests_without_usage", Model.requestsWithoutUsage usage)] )
