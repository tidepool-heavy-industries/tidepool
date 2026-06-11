-- | M1: baseline — no pragma, no QQ import, no splice.
module M1Plain where

import qualified Data.Text as T

result :: T.Text
result = T.toUpper (T.pack "hello") <> T.replicate 3 (T.pack "ab")
