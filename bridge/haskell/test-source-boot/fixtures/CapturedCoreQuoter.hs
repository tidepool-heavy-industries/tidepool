{-# LANGUAGE TemplateHaskellQuotes #-}
module CapturedCoreQuoter (answer) where

import Control.Monad (forM_)
import Language.Haskell.TH (Exp(..), Lit(..), runIO)
import Language.Haskell.TH.Quote (QuasiQuoter(..))
import MetadataQuoteSupport (answerValue)
import System.Directory (copyFile)

answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \settings -> do
      let (counter, restoration) = read settings :: (FilePath, Maybe (FilePath, FilePath))
      runIO (appendFile counter (show answerValue ++ "\n"))
      runIO (forM_ restoration (uncurry copyFile))
      pure (LitE (IntegerL answerValue))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
