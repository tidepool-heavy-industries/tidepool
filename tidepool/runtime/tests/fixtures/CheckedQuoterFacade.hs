module CheckedQuoterFacade (bash, describe) where

import Tidepool.Command.Types (Command)
import qualified Tidepool.Command.Types as Command
import Tidepool.Effects.Core (CommandSpec)
import Tidepool.QQ.Bash (bash)

{-# OPAQUE describe #-}
describe :: Command -> CommandSpec
describe = Command.describe
