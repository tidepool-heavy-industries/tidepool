module RecoveredExecutionStackCaller where

import GHC.Internal.ExecutionStack.Internal (Location, showLocation)

{-# OPAQUE caller #-}
caller :: Location -> ShowS
caller = showLocation
