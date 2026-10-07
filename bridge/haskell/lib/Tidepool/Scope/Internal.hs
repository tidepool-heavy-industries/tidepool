{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}

-- | Compiler substrate for the runtime-owned lexical scope boundary.
module Tidepool.Scope.Internal (runScope, runScopeSited) where

import Prelude
import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Effects.Core (ResourceScopes (..), ScopeFailure, CleanupError)
import Tidepool.Internal.RequestSite (RequestSite)

-- The compiler issues evidence for the runtime's status reply separately from
-- the arbitrary body result retained by withScope's exit cell.
{-# OPAQUE runScope #-}
runScope
  :: Member ResourceScopes effects
  => (Int -> Eff effects ())
  -> Eff effects (Either ScopeFailure (), Either CleanupError ())
runScope = runScopeSited (error "runScope: extractor must assign a typed site")

{-# OPAQUE runScopeSited #-}
runScopeSited
  :: Member ResourceScopes effects
  => RequestSite '[] (Either ScopeFailure (), Either CleanupError ())
  -> (Int -> Eff effects ())
  -> Eff effects (Either ScopeFailure (), Either CleanupError ())
runScopeSited site body = send (ScopeRunWith site body)
