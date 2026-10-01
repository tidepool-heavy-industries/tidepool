{-# LANGUAGE TemplateHaskellQuotes #-}
module MetadataQuoter (answer) where

import Control.Concurrent (threadDelay)
import Language.Haskell.TH (Exp(..), Lit(..), runIO)
import Language.Haskell.TH.Quote (QuasiQuoter(..))
import MetadataQuoteSupport (answerValue)

answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \_ -> do
      runIO (writeFile "EXECUTION_CANCEL_MARKER" "started" >> threadDelay 10000000)
      pure (LitE (IntegerL answerValue))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
