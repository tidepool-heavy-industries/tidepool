{-# LANGUAGE GADTs #-}
module WatchReplyEvidence where

import Data.Text (Text)
import Tidepool.Actor ()
import Tidepool.Agent.Watch.Internal (Watches(..), AwaitPlan(..), AwaitNode(..))

{-# OPAQUE registerSingle #-}
registerSingle :: Text -> Watches Int
registerSingle label = RegisterWatchWith label (AwaitPlan [ReadyNode] 0)

{-# OPAQUE registerGrouped #-}
registerGrouped :: Text -> Watches Int
registerGrouped label = RegisterWatchWith label (AwaitPlan [ReadyNode, ReadyNode, AllNode 0 1] 2)
