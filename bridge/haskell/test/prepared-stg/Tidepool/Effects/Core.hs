{-# LANGUAGE ExplicitForAll #-}

module Tidepool.Effects.Core where

{-# OPAQUE runLLMTurn #-}
runLLMTurn :: forall answer. String -> Maybe answer
runLLMTurn _ = Nothing

runLLMTurnSited :: forall answer. Int -> String -> Maybe answer
runLLMTurnSited _ _ = Nothing
