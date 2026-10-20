{-# LANGUAGE GHC2024 #-}
module LocalRecord where
import Prelude
data Box = Box { field :: Int }
root box = field box
helper box = box { field = old }
