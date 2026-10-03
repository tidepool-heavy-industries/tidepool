data Input = HiddenReplacement Bool

make :: Bool -> Input
make = HiddenReplacement

project :: Input -> Bool
project (HiddenReplacement value) = value
