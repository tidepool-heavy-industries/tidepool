{-# LANGUAGE QuasiQuotes #-}
module SameCellImportSupport (answerValue) where

import SameCellImportQuoter (answer)

answerValue :: Int
answerValue = [answer|completed|]
