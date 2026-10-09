{-# LANGUAGE QuasiQuotes #-}
module CapturedCoreConsumer where

import CapturedCoreQuoter qualified as Original

__result :: Int
__result = [Original.answer|{{SNAPSHOT_SETTINGS}}|]
