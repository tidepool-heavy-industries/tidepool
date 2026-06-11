{-# LANGUAGE QuasiQuotes #-}
-- | M5: splice whose expansion references home-module + package names
-- via hygienic NameG (TemplateHaskellQuotes in the quoter).
module M5THQuote where

import qualified Data.Text as T
import UpperTH (upperTH)

result :: T.Text
result = [upperTH|hello|] <> T.pack "?"
