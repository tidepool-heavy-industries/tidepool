{-# LANGUAGE GADTs, OverloadedStrings #-}
module OriginalTextRequest where

import Data.Text (Text)
import qualified Tidepool.Effects.Core as Core

{-# OPAQUE request #-}
request :: Core.Forks (Either Text (Int, Text, [Text]))
request = Core.ForksBeginWith False "group" ["branch"]
