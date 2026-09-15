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

{-# NOINLINE unresolved #-}
unresolved :: forall a effects. Member Replies effects => String -> Eff effects a
unresolved = runLLMTurn @a

unrelated :: Int
unrelated = 42
