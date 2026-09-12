{-# OPTIONS_GHC -ihaskell/lib #-}

module ResponseCellProbe where

import Tidepool.Agent.Reply.Internal (Response)

data G = G

childLike :: IO (Response a)
childLike = error "compile-only child"

consume :: Response G -> Int
consume _ = 1

cell :: IO Int
cell = do
  response <- childLike
  pure (consume response)
