{-# LANGUAGE TypeApplications #-}
import Data.Text (Text)
import Tidepool.Agent.Reply (currentRequest, RequestScope(..), attemptReply, ReplyError(..))
scope <- currentRequest @Text @Bool
case scope of
  RequestActive handle _ -> do
    refused <- attemptReply handle True
    display (case refused of { Left ReplyToolRequired -> True; _ -> False })
  RequestUnavailable _ -> display False
