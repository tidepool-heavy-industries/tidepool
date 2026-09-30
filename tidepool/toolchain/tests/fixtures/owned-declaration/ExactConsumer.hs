{-# LANGUAGE TypeFamilies #-}
module ExactConsumer where

import Tidepool.Session.Lib.G4

downstream :: Int
downstream = if (True :: Chosen Int)
  then freshAnswer + answer
  else 0
