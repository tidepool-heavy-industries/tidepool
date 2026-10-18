{-# LANGUAGE DataKinds #-}
module OriginalTextConsumer where

import Control.Monad.Freer (Eff, send)
import qualified Tidepool.Effects.Core as Core
import OriginalTextRequest (request, siteRequest)
import Tidepool.Agent.Reply.Internal (Replies)

result :: Eff '[Core.ForkAgent Core.OpenAI] ()
result = send request >> pure ()

sitedResult :: Eff '[Replies] ()
sitedResult = siteRequest >> pure ()
