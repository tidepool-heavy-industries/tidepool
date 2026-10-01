{-# LANGUAGE QuasiQuotes #-}
module MetadataBashTarget where

import Tidepool.Command.Types (Command)
import Tidepool.QQ.Bash (bash)

__result :: Command
__result = [bash|printf headless-command-ok|]
