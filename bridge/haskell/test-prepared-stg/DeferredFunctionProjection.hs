{-# LANGUAGE BangPatterns #-}
{-# LANGUAGE PackageImports #-}

module DeferredFunctionProjection where

import "ghc-internal" GHC.Internal.ExecutionStack qualified as ExecutionStack
import "ghc-internal" GHC.Internal.ExecutionStack.Internal qualified as Internal
import "ghc-internal" GHC.Internal.Stack.CCS qualified as CCS
import "ghc-internal" GHC.Internal.Stack.CloneStack qualified as CloneStack

stackFramesBare = Internal.stackFrames

collectStackTraceBare = Internal.collectStackTrace

getStackTraceBare = ExecutionStack.getStackTrace

ccsToStringsBare = CCS.ccsToStrings

decodeStackEntriesBare = CloneStack.decode

-- Same occurrences in a source module are not compiler-library capabilities.
stackFrames value = value
{-# NOINLINE stackFrames #-}

collectStackTrace :: IO (Maybe ())
collectStackTrace = pure Nothing
{-# NOINLINE collectStackTrace #-}

collectStackTrace1 :: IO (Maybe Bool)
collectStackTrace1 = pure (Just True)
{-# NOINLINE collectStackTrace1 #-}

-- The worker is also named $wgo, but belongs to this source module.
decodeStackEntriesLookalike :: [Int] -> Int
decodeStackEntriesLookalike = go 0
  where
    go !total [] = total
    go !total (value : values) = go (total + value) values
{-# NOINLINE decodeStackEntriesLookalike #-}
