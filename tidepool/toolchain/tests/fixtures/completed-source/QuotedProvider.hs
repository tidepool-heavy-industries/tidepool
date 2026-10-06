module QuotedProvider (capture) where

import Language.Haskell.TH (Exp(..), Lit(..))
import Language.Haskell.TH.Quote (QuasiQuoter(..))
import Language.Haskell.TH.Syntax (runIO)
import System.IO (readFile')

capture :: QuasiQuoter
capture = QuasiQuoter
  { quoteExp = \path -> do
      input <- runIO (readFile' path)
      runIO (appendFile (path ++ ".executions") (input ++ "\n"))
      pure (LitE (IntegerL (read input)))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
