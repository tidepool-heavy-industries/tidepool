{-# LANGUAGE GADTs #-}
import qualified DuplicateFields as Full (One(..))
data One where
  LocalOne :: Int -> One
let local = LocalOne 17
let imported = Full.One 42
same imported
