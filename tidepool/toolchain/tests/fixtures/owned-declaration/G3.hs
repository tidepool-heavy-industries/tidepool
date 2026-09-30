{-# LANGUAGE TypeFamilies #-}
module Tidepool.Session.Lib.G3
  (module Tidepool.Session.Lib.G2, freshAnswer) where

import Tidepool.Session.Lib.G2

freshAnswer :: Int
freshAnswer = if (True :: Chosen Int)
  then answer + choose (0 :: Int)
  else 0
