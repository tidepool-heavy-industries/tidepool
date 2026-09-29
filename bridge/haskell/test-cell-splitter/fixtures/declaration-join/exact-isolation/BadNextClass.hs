{-# LANGUAGE FlexibleContexts #-}
module BadNextClass where
import Next
needsOld :: C Bool => Int
needsOld = 0
bad :: Int
bad = needsOld
