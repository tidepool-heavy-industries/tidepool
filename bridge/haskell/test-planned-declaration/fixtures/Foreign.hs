module Foreign (Box(..), Remaining(..), ForeignRecord(..), (<+>), hidden) where

data Box = Box Int
data Remaining = Taken Int | Keep
data ForeignRecord = ForeignRecord { common :: Int, untouched :: Int }
(<+>) :: Int -> Int -> Int
(<+>) = (+)
hidden :: Int
hidden = 71
