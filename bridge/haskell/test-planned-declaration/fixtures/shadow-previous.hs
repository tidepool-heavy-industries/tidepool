{-# LANGUAGE DuplicateRecordFields #-}
module Tidepool.Session.Lib.G6
  ( Input(..), make, project, Tagged(..), Record(..), OtherRecord(..), OldConstructor(..) ) where

data Input = HiddenOriginal Bool
make :: Bool -> Input
make = HiddenOriginal
project :: Input -> Bool
project (HiddenOriginal value) = value

class Tagged a where
  tag :: a -> Bool
instance Tagged Input where
  tag = project

data Record = Record { field :: Int }
data OtherRecord = OtherRecord { field :: Bool }
data OldConstructor = ConstructorOnly Bool

__result :: IO ()
__result = pure ()
