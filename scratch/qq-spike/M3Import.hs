{-# LANGUAGE QuasiQuotes #-}
-- | M3: pragma + quoter module imported, but NO splice used.
-- This is the post-Phase-2 world for evals that never touch QQ.
module M3Import where

import qualified Data.Text as T
import UpperQQ (upper)

result :: T.Text
result = T.toUpper (T.pack "hello") <> T.replicate 3 (T.pack "ab")
