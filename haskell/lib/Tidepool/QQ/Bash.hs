{-# LANGUAGE TemplateHaskellQuotes #-}
module Tidepool.QQ.Bash (bash) where

import Language.Haskell.TH (Exp (..), Lit (..))
import Language.Haskell.TH.Quote (QuasiQuoter (..))
import qualified Tidepool.Data.Text as T
import Tidepool.Command.Types (bashCommand)

-- | Literal Bash source. Haskell interpolates nothing into the quotation:
-- @$VAR@ and backticks mean what Bash means. Dynamic Haskell values are passed
-- with 'Tidepool.Command.withArguments', whose list positions appear inside the
-- quotation as the positional parameters @$1@, @$2@, ... in order, so
-- @withArguments [old, new] [bash|git diff --stat "$1" "$2"|]@ reads @old@ as
-- @$1@ and @new@ as @$2@. Never concatenate a value into the script text.
bash :: QuasiQuoter
bash = QuasiQuoter
  { quoteExp = \source -> pure (AppE (VarE 'bashCommand) (AppE (VarE 'T.pack) (LitE (StringL source))))
  , quotePat = \_ -> fail "bash constructs a Command expression"
  , quoteType = \_ -> fail "bash constructs a Command expression"
  , quoteDec = \_ -> fail "bash constructs a Command expression"
  }
