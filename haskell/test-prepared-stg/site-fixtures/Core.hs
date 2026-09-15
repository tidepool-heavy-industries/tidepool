{-# LANGUAGE DataKinds, FlexibleContexts, KindSignatures #-}
{-# LANGUAGE ExplicitForAll, TypeApplications, TypeOperators #-}
-- | The production verb shape over freer-simple's 'Member'. Its instances
-- resolve `Member V (W ': effects)` from a given `Member V effects`, and GHC
-- wraps such a call in 'GHC.Magic.nospec'.
module Tidepool.Effects.Core
  ( Replies, Other, Member, Eff, runLLMTurn, runLLMTurnSited ) where

import Control.Monad.Freer (Eff, Member)
import Data.Kind (Type)

data Replies (a :: Type)
data Other (a :: Type)

{-# OPAQUE runLLMTurn #-}
runLLMTurn :: forall a effects. Member Replies effects => String -> Eff effects a
runLLMTurn _ = error "surface verb is rewritten to its site-aware sibling"
{-# OPAQUE runLLMTurnSited #-}
runLLMTurnSited :: forall a effects. Member Replies effects => Int -> String -> Eff effects a
runLLMTurnSited _ _ = error "site-aware sibling is not executed by this test"
