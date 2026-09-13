{-# LANGUAGE PackageImports #-}

module DeferredFunctionProjection where

import "ghc-internal" GHC.Internal.ExecutionStack qualified as ExecutionStack
import "ghc-internal" GHC.Internal.ExecutionStack.Internal qualified as Internal
import "ghc-internal" GHC.Internal.Stack.CCS qualified as CCS

stackFramesBare = Internal.stackFrames

collectStackTraceBare = Internal.collectStackTrace

getStackTraceBare = ExecutionStack.getStackTrace

ccsToStringsBare = CCS.ccsToStrings

-- Same occurrences in a source module are not compiler-library capabilities.
stackFrames value = value
{-# NOINLINE stackFrames #-}

collectStackTrace :: IO (Maybe ())
collectStackTrace = pure Nothing
{-# NOINLINE collectStackTrace #-}

collectStackTrace1 :: IO (Maybe Bool)
collectStackTrace1 = pure (Just True)
{-# NOINLINE collectStackTrace1 #-}
