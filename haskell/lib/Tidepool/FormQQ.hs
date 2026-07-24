{-# LANGUAGE TemplateHaskellQuotes #-}

-- | The @[form|...|]@ quasi-quoter: a line-based DSL compiling to @[Ui]@ —
-- one widget per non-blank line, via the "Tidepool.Ui" smart constructors.
-- Sibling of the "Tidepool.QQ" quoters (@[fmt|]@\/@[j|]@\/@[patch|]@\/@[uri|]@);
-- lives at the top level (not under @Tidepool.QQ@) since it targets @Ui@
-- rather than the eval-dialect literal types those cover.
--
-- == Grammar
--
-- @
-- choice \<prompt\>: \<key\> \<key\> ...   -- 'Tidepool.Ui.choice'; each key is both key and label
-- text \<prompt\>                        -- 'Tidepool.Ui.textIn' prompt False
-- multiline \<prompt\>                   -- 'Tidepool.Ui.textIn' prompt True
-- \<anything else\>                      -- 'Tidepool.Ui.prose', verbatim
-- @
--
-- Blank lines (all whitespace) are skipped. Line parsing
-- ('Tidepool.FormQQ.Parse.parseFormLine') is pure and unit-tested directly;
-- this module only wires it into the splice evaluator. Same mechanism as
-- every other tidepool quoter: parsing happens entirely at COMPILE time
-- (inside the splice evaluator, full GHC available), expanding to plain Core
-- over 'Data.Text.Text' and 'Tidepool.Ui.Ui' constructor applications — no
-- runtime parsing, nothing the Cranelift JIT doesn't already run.
--
-- The quoter expands to a plain @[Ui]@ list (not a 'Card'-wrapped value —
-- the grammar has no title), so @card "title" [form|...|]@ is the idiomatic
-- use. A malformed line is a COMPILE-TIME error naming the offending
-- 1-indexed line number.
module Tidepool.FormQQ (form) where

import Prelude
import Language.Haskell.TH        (Exp (..), Q, litE, stringL, listE, tupE)
import Language.Haskell.TH.Quote  (QuasiQuoter (..))
import qualified Tidepool.Data.Text as T
import Tidepool.Ui                (choice, textIn, prose)
import Tidepool.FormQQ.Parse       (ParsedLine (..), parseFormLine)

-- | @[form|...|]@ — see the module haddock for grammar.
form :: QuasiQuoter
form = QuasiQuoter
  { quoteExp  = formExp
  , quotePat  = \_ -> fail "form: cannot be used in pattern position"
  , quoteType = \_ -> fail "form: cannot be used in type position"
  , quoteDec  = \_ -> fail "form: cannot be used in declaration position"
  }

formExp :: String -> Q Exp
formExp src = do
  widgetLists <- mapM lineExp (zip [1 :: Int ..] (lines src))
  return (ListE (concat widgetLists))

-- | One source line -> zero (blank) or one (widget) 'Exp'; a malformed line
-- fails the splice with the 1-indexed line number.
lineExp :: (Int, String) -> Q [Exp]
lineExp (n, raw) = case parseFormLine raw of
  Left err        -> fail ("form: line " ++ show n ++ ": " ++ err)
  Right Nothing   -> return []
  Right (Just pl) -> (: []) <$> widgetExp pl

widgetExp :: ParsedLine -> Q Exp
widgetExp (PChoice p keys) =
  [| choice $(textLit p) $(listE [ tupE [textLit k, textLit k] | k <- keys ]) |]
widgetExp (PText p)      = [| textIn $(textLit p) False |]
widgetExp (PMultiline p) = [| textIn $(textLit p) True |]
widgetExp (PProse l)     = [| prose $(textLit l) |]

-- | A source 'String' as a 'Data.Text.Text' literal.
textLit :: String -> Q Exp
textLit s = [| T.pack $(litE (stringL s)) |]
