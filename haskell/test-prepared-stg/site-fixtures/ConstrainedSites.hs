{-# LANGUAGE DataKinds, FlexibleContexts, ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications, TypeOperators #-}
module ConstrainedSites where

import Tidepool.Effects.Core

{-# NOINLINE partial #-}
partial :: forall effects. Member Replies (Other ': effects) => String -> Eff (Other ': effects) Bool
partial = runLLMTurn @Bool

{-# NOINLINE wrapped #-}
wrapped :: forall effects. Member Replies effects => String -> Eff effects Bool
wrapped = runLLMTurn @Bool @effects

{-# NOINLINE higherOrder #-}
higherOrder :: forall effects. Member Replies effects => [String] -> [Eff effects Bool]
higherOrder = map (runLLMTurn @Bool @effects)

-- The dictionary for the open-tail row is built from the given one through
-- freer-simple's instances; GHC wraps these calls in `nospec`.
{-# NOINLINE openTail #-}
openTail :: forall effects. Member Replies effects => Eff (Other ': effects) Bool
openTail = runLLMTurn @Bool "open"

{-# NOINLINE openEta #-}
openEta :: forall effects. Member Replies effects => String -> Eff (Other ': effects) Bool
openEta = runLLMTurn @Bool

{-# NOINLINE unresolved #-}
unresolved :: forall a effects. Member Replies effects => String -> Eff effects a
unresolved = runLLMTurn @a

unrelated :: Int
unrelated = 42
