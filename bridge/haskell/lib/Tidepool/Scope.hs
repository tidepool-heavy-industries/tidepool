{-# LANGUAGE FlexibleContexts #-}

-- | Lexical cleanup ownership interpreted by the resident runtime.
module Tidepool.Scope
  ( Scope
  , ScopeFailure (..)
  , CleanupError (..)
  , ScopeOutcome (..)
  , withScope
  ) where

import Prelude
import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Effects.Core
  ( ResourceScopes (..)
  , Scope (..)
  , ScopeFailure (..)
  , CleanupError (..)
  )
import Tidepool.Internal.ExitCell (fillExitCell, newExitCell, readExitCell)

-- | A body can return a value even when external cleanup remains unconfirmed.
-- Cleanup uncertainty never replaces the body's result or its original failure.
data ScopeOutcome a = ScopeOutcome
  { scopeBody :: Either ScopeFailure a
  , scopeCleanup :: Either CleanupError ()
  }
  deriving (Show, Eq)

-- | Run a callback under a runtime-issued cleanup owner. Resources opt in with
-- @InScope scope@. The runtime closes admission and retains finalization on
-- every exit, including evaluation failure and cancellation. An escaped token
-- cannot admit new resources after closure.
--
-- The callback publishes its live value into parent-retained Haskell storage
-- before marking completion. The marker does not perform cleanup: the runtime
-- owns finalization, including exits that never reach the marker. If the outer
-- invocation is cancelled, its owner retains cleanup without promising a reply.
withScope
  :: Member ResourceScopes effects
  => (Scope -> Eff effects a)
  -> Eff effects (ScopeOutcome a)
{-# NOINLINE withScope #-}
withScope body = do
  let cell = newExitCell body
      publish token = do
        value <- body (ScopeToken token)
        case fillExitCell cell value of
          () -> send (ScopeDoneWith token)
  (bodyStatus, cleanupStatus) <- send (ScopeRunWith publish)
  let bodyResult = case bodyStatus of
        Left failure -> Left failure
        Right () -> case readExitCell bodyStatus cell of
          Just value -> Right value
          Nothing -> error "Tidepool.Scope.withScope: completed body has an empty exit cell"
  pure (ScopeOutcome bodyResult cleanupStatus)
