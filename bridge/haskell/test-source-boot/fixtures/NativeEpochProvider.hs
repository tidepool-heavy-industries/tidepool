{-# LANGUAGE ForeignFunctionInterface, TemplateHaskell #-}
module NativeEpochProvider (nativeType) where

import Foreign.C.Types (CInt(..))
import Language.Haskell.TH
import Language.Haskell.TH.Syntax (ForeignSrcLang(LangC), addForeignSource)

$(addForeignSource LangC "int tidepool_epoch_value(void) { return EPOCH_VALUE; }\n" >> pure [])

foreign import ccall unsafe "tidepool_epoch_value" nativeValue :: IO CInt
foreign import ccall unsafe "getpid" nativePid :: IO CInt

nativeType :: Q Type
nativeType = do
  value <- runIO nativeValue
  pid <- runIO nativePid
  runIO (appendFile "EPOCH_MARKER" (show value ++ " " ++ show pid ++ "\n"))
  pure (LitT (NumTyLit (fromIntegral value)))
