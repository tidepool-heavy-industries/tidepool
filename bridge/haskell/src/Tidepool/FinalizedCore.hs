-- | Source-free native Core encoding and checked recovery of finalized modules.
module Tidepool.FinalizedCore
  ( FinalizedCoreFailure(..), UnsupportedCoreMetadata(..), isUnsupportedFinalizedCore
  , captureFinalizedCore, decodeFinalizedCore, attachFinalizedCore, finalizedCoreSiteInstances
  ) where

import Tidepool.FinalizedModule.Internal
  ( FinalizedCoreFailure(..), UnsupportedCoreMetadata(..), isUnsupportedFinalizedCore
  , captureFinalizedCore, decodeFinalizedCore, attachFinalizedCore, finalizedCoreSiteInstances )
