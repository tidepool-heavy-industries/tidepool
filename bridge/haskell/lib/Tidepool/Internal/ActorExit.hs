-- | Typed lifecycle observations, independent of the generated effect declarations.
module Tidepool.Internal.ActorExit
  ( ActorExit (..), ActorFailure (..), CancelReason (..), ActorObservationFailure (..) ) where

import Data.Text (Text)
import Prelude

newtype ActorFailure = ActorFailure { actorFailureSummary :: Text }
  deriving (Show, Eq)

newtype CancelReason = CancelReason { cancelReasonSummary :: Text }
  deriving (Show, Eq)

-- | The observer could not obtain the retained value of this exact incarnation.
newtype ActorObservationFailure = ActorObservationFailure { actorObservationFailureSummary :: Text }
  deriving (Show, Eq)

data ActorExit exit
  = Completed exit
  | Failed ActorFailure
  | Cancelled CancelReason
  | Unavailable ActorObservationFailure
  deriving (Show, Eq)
