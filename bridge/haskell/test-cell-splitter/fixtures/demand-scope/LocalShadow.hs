{-# LANGUAGE GHC2024 #-}
module LocalShadow where
import Prelude
root = (let old = 1 in old, (\old -> old) 1, case 1 of { old -> old }, do { old <- pure 1; pure old })
