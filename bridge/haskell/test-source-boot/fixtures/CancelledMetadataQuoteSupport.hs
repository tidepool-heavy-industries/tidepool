{-# LANGUAGE TemplateHaskell #-}
module MetadataQuoteSupport (answerValue) where

import Control.Concurrent (threadDelay)
import Language.Haskell.TH.Syntax (runIO)

$(runIO (writeFile "CANCELLED_HISTORY_MARKER" "started" >> threadDelay 60000000) >> pure [])

answerValue :: Integer
answerValue = 99
