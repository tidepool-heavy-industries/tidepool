{-# LANGUAGE PackageImports #-}
module RetainedReceiveConsumer where

import qualified RetainedReceiveOwner as Original
import "tidepool-resume" Tidepool.Internal.Resume (settle, resumeLifted)

__prepared = settle Original.result
__resume q x = settle (resumeLifted q x)
