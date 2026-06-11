{-# LANGUAGE QuasiQuotes #-}
-- | M4: pragma + import + an actual splice.
module M4Splice where

import qualified Data.Text as T
import UpperQQ (upper)

result :: T.Text
result = T.pack [upper|hello|] <> T.replicate 3 (T.pack "ab")
