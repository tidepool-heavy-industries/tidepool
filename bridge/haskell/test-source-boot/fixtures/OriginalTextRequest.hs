{-# LANGUAGE DataKinds, GADTs, OverloadedStrings, ScopedTypeVariables, TypeApplications #-}
module OriginalTextRequest (request, siteRequest, independent, invalid) where

import Data.Text (Text)
import Control.Monad.Freer (Eff)
import Tidepool.Agent.Reply.Internal (Replies, RequestScope, currentRequest)
import qualified Tidepool.Effects.Core as Core

{-# OPAQUE request #-}
request :: Core.AgentLaunch (Either Core.CheckpointRefusal Text)
request = Core.AgentLaunchCheckpointWith "metadata"

{-# OPAQUE siteRequest #-}
siteRequest :: Eff '[Replies] (RequestScope Text Int)
siteRequest = continue (\() -> currentRequest @Text @Int)

{-# OPAQUE continue #-}
continue :: (() -> Eff effects a) -> Eff effects a
continue action = action ()

{-# OPAQUE independent #-}
independent :: Eff '[Replies] (RequestScope Text Int)
independent = continue (\() -> currentRequest @Text @Int)

{-# OPAQUE invalid #-}
invalid :: forall input result. Eff '[Replies] (RequestScope input result)
invalid = continue (\() -> currentRequest @input @result)
