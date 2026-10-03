{-# LANGUAGE DataKinds #-}

module ModuleProductCatalogA
  ( SafeTag(..), safeValue, rawProgress, dependentProgress ) where

import Control.Monad.Freer (Eff, send)
import Tidepool.Agent.Reply.Internal (ReplyError, Replies(..))

data SafeTag = SafeTag

{-# NOINLINE safeValue #-}
safeValue :: Int
safeValue = if cycleEven 2 then 42 else 0

{-# NOINLINE cycleEven #-}
cycleEven :: Int -> Bool
cycleEven n = n == 0 || cycleOdd (n - 1)

{-# NOINLINE cycleOdd #-}
cycleOdd :: Int -> Bool
cycleOdd n = n /= 0 && cycleEven (n - 1)

{-# NOINLINE rawProgress #-}
rawProgress :: Eff '[Replies] (Either ReplyError ())
rawProgress = send (PublishProgressWith (-1) (1 :: Int))

{-# NOINLINE dependentProgress #-}
dependentProgress :: Eff '[Replies] (Either ReplyError ())
dependentProgress = rawProgress
