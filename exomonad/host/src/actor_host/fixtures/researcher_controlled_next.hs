{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
import Data.Text (Text)
import qualified Tidepool.Actors.Exomonad as Agent
import qualified Data.Text as Text
import qualified Project.Researcher as Researcher
import Project.PositiveReport (UnsupportedReport)
unsupported <- requestWithReplyTool @UnsupportedReport researcher ("Do not publish an unsupported reply schema" :: Text) defaultRequestOptions
display (case unsupported of { Left (RequestReplyToolInvalid _) -> True; _ -> False })
Right followUpRequest <- requestWithReplyTool @Researcher.FollowUp researcher
  ("Explain the same source in one sentence and cite its URL." :: Text)
  defaultRequestOptions
followUp <- either (error . Text.pack . show) pure =<< await (Agent.result followUpRequest)
display followUp
