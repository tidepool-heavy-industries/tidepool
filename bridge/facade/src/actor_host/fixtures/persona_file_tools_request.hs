{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
import Data.Text (Text)
Right fileRepair <- requestWithReplyTool @Bool fileCompanion
  ("First observe the notebook typecheck refusal, then create, replace, preview and repair the draft through the installed tools." :: Text) defaultRequestOptions
Right fileRepaired <- await (result fileRepair)
display fileRepaired
