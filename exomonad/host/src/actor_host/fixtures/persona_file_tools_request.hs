{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
import Data.Text (Text)
import qualified Tidepool.Actors.Exomonad as Agent
import qualified Data.Text as Text
Right fileRepair <- requestWithReplyTool @Bool fileCompanion
  ("First observe the notebook typecheck refusal, then create, replace, preview and repair the draft through the installed tools." :: Text) defaultRequestOptions
fileRepaired <- either (error . Text.pack . show) pure =<< await (Agent.result fileRepair)
display fileRepaired
