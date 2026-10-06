{-# LANGUAGE ForeignFunctionInterface, TemplateHaskell #-}
module NativeEpochMissing (missingType) where

import Foreign.C.Types (CInt(..))
import Language.Haskell.TH

foreign import ccall unsafe "tidepool_epoch_missing_required" missingValue :: IO CInt

missingType :: Q Type
missingType = do
  value <- runIO missingValue
  pure (LitT (NumTyLit (fromIntegral value)))
