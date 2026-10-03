{-# LANGUAGE DataKinds #-}

module ModuleProductCatalogB (consumeSafe, consumeDependentProgress) where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Reply.Internal (ReplyError, Replies)
import ModuleProductCatalogA (dependentProgress, safeValue)

{-# NOINLINE consumeSafe #-}
consumeSafe :: Int
consumeSafe = safeValue + 1

{-# NOINLINE consumeDependentProgress #-}
consumeDependentProgress :: Eff '[Replies] (Either ReplyError ())
consumeDependentProgress = dependentProgress
