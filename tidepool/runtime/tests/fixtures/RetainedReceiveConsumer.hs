module RetainedReceiveConsumer where

import qualified RetainedReceiveOwner as Original
import Tidepool.Internal.Resume (settle)

__prepared = settle Original.result
