{-# LANGUAGE MagicHash #-}

module FingerprintExecutionContract where

import GHC.Exts (Addr#)
import GHC.Ptr (Ptr (..))
import GHC.Utils.Fingerprint (Fingerprint (..), fingerprintData)
import System.IO.Unsafe (unsafeDupablePerformIO)

-- Unlike resident cells this standalone oracle does not need EVAL_PRAGMAS:
-- every binding is monomorphic, uses ordinary Prelude syntax, and exercises
-- only the pinned GHC fingerprint implementation. Literal addresses keep the
-- contract focused on fingerprintData rather than String encoding helpers.
fingerprintWords :: Addr# -> Int -> (Int, Int)
fingerprintWords address length =
  case unsafeDupablePerformIO (fingerprintData (Ptr address) length) of
    Fingerprint high low -> (fromIntegral high, fromIntegral low)

emptyFingerprint :: (Int, Int)
emptyFingerprint = fingerprintWords ""# 0

abcFingerprint :: (Int, Int)
abcFingerprint = fingerprintWords "abc"# 3

multiblockFingerprint :: (Int, Int)
multiblockFingerprint =
  fingerprintWords
    "12345678901234567890123456789012345678901234567890123456789012345678901234567890"#
    80
