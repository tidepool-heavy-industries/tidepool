{-# LANGUAGE DataKinds, TypeApplications #-}
{-# OPTIONS_GHC -O1 #-}
module SiteComponentFixture (answerA, answerB) where

import Control.Monad.Freer (Eff)
import Tidepool.Actor (receive)
import Tidepool.Scope.Internal (runScope)

data Chain = End | Link Chain
data Shared = Shared !Int Chain

-- Distinct private components overlap in nominal evidence. The local strings
-- require CorePrep to emit tops alongside each original site-bearing body.
{-# OPAQUE answerA #-}
answerA :: Int -> Maybe (Either Shared Chain)
answerA n = receive @(Either Shared Chain) (privateA n)

{-# NOINLINE privateA #-}
privateA :: Int -> String
privateA n | n <= 0 = "component A"
           | otherwise = privateA (n - 1)

{-# OPAQUE answerB #-}
answerB :: Int -> Eff '[] (Either Chain Shared)
answerB n = runScope (privateB n :: Either Chain Shared)

{-# NOINLINE privateB #-}
privateB :: Int -> Either Chain Shared
privateB n | n <= 0 = Left End
           | otherwise = privateB (n - 1)
