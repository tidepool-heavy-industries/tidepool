module Tidepool.Session.Lib.G2 where
import Tidepool.Session.Lib.G1 (baseline)
import PrefixFreshSupport (fresh)
answer = baseline + fresh
{-# NOINLINE answer #-}
