{-# LANGUAGE DataKinds, FlexibleContexts, MultiParamTypeClasses #-}
{-# LANGUAGE ExplicitForAll, KindSignatures, TypeApplications, TypeOperators #-}
module Tidepool.Effects.Core where

import Data.Kind (Type)

data Replies
data Other
class Member effect (effects :: [Type])
data Eff (effects :: [Type]) (a :: Type) = Done

{-# OPAQUE runLLMTurn #-}
runLLMTurn :: forall a effects. Member Replies effects => String -> Eff effects a
runLLMTurn _ = Done
{-# OPAQUE runLLMTurnSited #-}
runLLMTurnSited :: forall a effects. Member Replies effects => Int -> String -> Eff effects a
runLLMTurnSited _ _ = Done
