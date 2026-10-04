{-# LANGUAGE DuplicateRecordFields #-}
data Input = HiddenReplacement Bool
make :: Bool -> Input
make = HiddenReplacement
project :: Input -> Bool
project (HiddenReplacement value) = value

class Tagged a where
  tag :: a -> Bool
instance Tagged Input where
  tag = project

data Record = Record { field :: Bool }
data ConstructorOnly = FreshConstructorOnly
data Maybe = LocalMaybe
data Box = LocalBox

historical :: Tidepool.Session.Lib.G6.Input -> Bool
historical = Tidepool.Session.Lib.G6.project
historicalMaybe :: Prelude.Maybe Bool
historicalMaybe = Just True
historicalBox :: Selected.Box
historicalBox = Selected.Box 42
historicalConstructor :: OldConstructor
historicalConstructor = ConstructorOnly True
historicalField :: OtherRecord
historicalField = OtherRecord { field = True }
local :: Bool
local = tag (make True)
