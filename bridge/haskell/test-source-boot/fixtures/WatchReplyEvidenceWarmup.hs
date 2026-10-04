{-# LANGUAGE GADTs #-}
module WatchReplyEvidence where

import Data.Text (Text)
import Tidepool.Agent.Watch.Internal (Watches(..))

{-# OPAQUE registerSingle #-}
registerSingle :: Text -> Watches Int
registerSingle label = RegisterWatchWith label []

{-# OPAQUE registerGrouped #-}
registerGrouped :: Text -> Watches Int
registerGrouped label = RegisterWatchGroupsWith label []
