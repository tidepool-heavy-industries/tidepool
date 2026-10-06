{-# LANGUAGE DataKinds, GADTs, OverloadedStrings, TypeApplications #-}
module OriginalTextRequest where

import Data.Text (Text)
import Control.Monad.Freer (Eff)
import Tidepool.Agent.Reply.Internal (Replies, RequestScope, currentRequest)
import qualified Tidepool.Effects.Core as Core

{-# OPAQUE request #-}
request :: Core.Forks (Either Text (Int, Text, [Text]))
request = Core.ForksBeginWith False "group" ["branch"]

{-# OPAQUE siteRequest #-}
siteRequest :: Eff '[Replies] (RequestScope Text Int)
siteRequest = continue (\() -> currentRequest @Text @Int)

{-# OPAQUE continue #-}
continue :: (() -> Eff '[Replies] (RequestScope Text Int)) -> Eff '[Replies] (RequestScope Text Int)
continue action = action ()
