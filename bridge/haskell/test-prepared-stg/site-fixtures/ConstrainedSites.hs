{-# LANGUAGE DataKinds, FlexibleContexts, ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications, TypeOperators #-}
module ConstrainedSites where

import Tidepool.Actor

{-# NOINLINE partial #-}
partial :: forall effects. Member Replies (Other ': effects) => String -> Eff (Other ': effects) Bool
partial = receive @Bool

{-# NOINLINE wrapped #-}
wrapped :: forall effects. Member Replies effects => String -> Eff effects Bool
wrapped = receive @Bool @effects

{-# NOINLINE higherOrder #-}
higherOrder :: forall effects. Member Replies effects => [String] -> [Eff effects Bool]
higherOrder = map (receive @Bool @effects)

-- The dictionary for the open-tail row is built from the given one through
-- freer-simple's instances; GHC wraps these calls in `nospec`.
{-# NOINLINE openTail #-}
openTail :: forall effects. Member Replies effects => Eff (Other ': effects) Bool
openTail = receive @Bool "open"

{-# NOINLINE openEta #-}
openEta :: forall effects. Member Replies effects => String -> Eff (Other ': effects) Bool
openEta = receive @Bool

{-# NOINLINE unresolved #-}
unresolved :: forall a effects. Member Replies effects => String -> Eff effects a
unresolved = receive @a

unrelated :: Int
unrelated = 42
