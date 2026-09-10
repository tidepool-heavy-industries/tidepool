{-# LANGUAGE TemplateHaskellQuotes #-}
module Tidepool.QQ.Bash (bash) where

import Language.Haskell.TH (Exp (..), Lit (..))
import Language.Haskell.TH.Quote (QuasiQuoter (..))
import qualified Tidepool.Data.Text as T
import Tidepool.Command.Types (bashCommand)

-- | Literal Bash source. Use positional arguments for dynamic Haskell values.
bash :: QuasiQuoter
bash = QuasiQuoter
  { quoteExp = \source -> pure (AppE (VarE 'bashCommand) (AppE (VarE 'T.pack) (LitE (StringL source))))
  , quotePat = \_ -> fail "bash constructs a Command expression"
  , quoteType = \_ -> fail "bash constructs a Command expression"
  , quoteDec = \_ -> fail "bash constructs a Command expression"
  }
