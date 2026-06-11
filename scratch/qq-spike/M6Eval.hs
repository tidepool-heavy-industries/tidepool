{-# LANGUAGE QuasiQuotes #-}
-- | Probe A: eval-mode (target) translation of a binding that mixes a QQ
-- splice with Double arithmetic. Does the clz# poisoning hit eval mode?
module M6Eval where

import qualified Data.Text as T
import UpperQQ (upper)

result :: T.Text
result = T.pack [upper|hi|] <> T.pack (show (1.5 + 2.25 :: Double))
