{-# LANGUAGE DataKinds, TypeApplications #-}
module SiteComponentFixture (answerA, answerB) where

import Control.Monad.Freer (Eff)
import Tidepool.Actor (receive)
import Tidepool.Scope.Internal (runScope)

data Chain = End | Link Chain
data Shared = Shared !Int Chain

-- Distinct private components overlap in nominal evidence. CAF site calls
-- leave generated carrier arguments for CorePrep to float beside their tops.
{-# OPAQUE answerA #-}
answerA :: Maybe (Either Shared Chain)
answerA = receive @(Either Shared Chain) (privateA 7)

{-# NOINLINE privateA #-}
privateA :: Int -> String
privateA n | n <= 0 = "component A"
           | otherwise = privateA (n - 1)

{-# OPAQUE answerB #-}
answerB :: Eff '[] (Either Chain Shared)
answerB = runScope (privateB 7 :: Either Chain Shared)

{-# NOINLINE privateB #-}
privateB :: Int -> Either Chain Shared
privateB n | n <= 0 = Left End
           | otherwise = privateB (n - 1)
