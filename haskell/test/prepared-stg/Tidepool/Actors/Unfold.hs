{-# LANGUAGE ExplicitForAll #-}

module Tidepool.Actors.Unfold where

child :: forall result child effects input. input -> Maybe result
child _ = Nothing

childSited :: forall result child effects input. Int -> input -> Maybe result
childSited _ _ = Nothing
