{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnliftedFFITypes #-}

module CStringLengthProjection where

import GHC.CString (cstringLength#)
import GHC.Exts (Addr#, Int(I#), Word(W#), Word#)

lengthOf :: Addr# -> Int
lengthOf address = I# (cstringLength# address)

foreign import ccall unsafe "strlen" wrongLength# :: Addr# -> Word#

wrongLength :: Addr# -> Word
wrongLength address = W# (wrongLength# address)
