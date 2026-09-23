{-# LANGUAGE MagicHash #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE UnliftedFFITypes #-}

module TextMemchrProjection where

import Data.Text (Text)
import qualified Data.Text as T
import Data.Text.Internal.Search (indices)
import GHC.Exts (ByteArray#, Word#, Word(W#), wordToWord8#, Word8#)

commaIndices :: Text -> [Int]
commaIndices haystack = indices "," haystack

foreign import ccall unsafe "_hs_text_memchr"
  wrongMemchr# :: ByteArray# -> Word# -> Word# -> Word8# -> Word#

wrongMemchr :: ByteArray# -> Word
wrongMemchr array = W# (wrongMemchr# array 0## 4## (wordToWord8# 0##))

-- `T.measureOff` is text's character-prefix measure; its recovered body calls
-- the `_hs_text_measure_off` kernel directly.
measureTwo :: Text -> Int
measureTwo = T.measureOff 2

breakPath :: Text -> (Text, Text)
breakPath = T.breakOnEnd "/"

foreign import ccall unsafe "_hs_text_measure_off"
  wrongMeasureOff# :: ByteArray# -> Word# -> Word# -> Word# -> Word#

wrongMeasureOff :: ByteArray# -> Word
wrongMeasureOff array = W# (wrongMeasureOff# array 0## 4## 1##)
