{-# LANGUAGE QuasiQuotes #-}
-- | M2: QuasiQuotes pragma enabled, but no QQ import and no splice.
module M2Pragma where

import qualified Data.Text as T

result :: T.Text
result = T.toUpper (T.pack "hello") <> T.replicate 3 (T.pack "ab")
