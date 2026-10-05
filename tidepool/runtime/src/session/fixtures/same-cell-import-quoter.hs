module SameCellImportQuoter (answer) where

import Language.Haskell.TH (Exp(..), Lit(..))
import Language.Haskell.TH.Quote (QuasiQuoter(..))
import Language.Haskell.TH.Syntax (runIO)

answer :: QuasiQuoter
answer = QuasiQuoter
  { quoteExp = \_ -> do
      runIO (appendFile SAME_CELL_IMPORT_MARKER "quoter\n")
      pure (LitE (IntegerL 42))
  , quotePat = \_ -> fail "expression only"
  , quoteType = \_ -> fail "expression only"
  , quoteDec = \_ -> fail "expression only"
  }
