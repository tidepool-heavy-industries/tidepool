module ActivationInputOriginal (Input, make, project) where

data Input = HiddenOriginal Int

make :: Int -> Input
make = HiddenOriginal

project :: Input -> Int
project (HiddenOriginal value) = value
