{-# LANGUAGE ForeignFunctionInterface #-}
module ActivationDisplayUnavailable () where

import qualified ActivationInputOriginal as Original
import qualified Data.Text as Text
import Foreign.C.Types (CInt (..))
import Tidepool.Inspection.Display (Display (..), WorkbenchDisplay (..))

instance Display Original.Input where
  displayWith budget value =
    let text = Text.pack (if Original.project value == 42 then "unavailable original display" else "wrong original input")
    in (Text.take budget text, Text.length text > budget)

instance WorkbenchDisplay Original.Input where
  workbenchDisplay = displayWith 65536
  workbenchActivationDisplay = displayWith

-- Foreign export stubs make this owner's finalized Core unsupported for
-- retained native projection. The request itself never calls this function.
foreign export ccall "activation_preview_unused_export" unusedExport :: CInt -> IO CInt

unusedExport :: CInt -> IO CInt
unusedExport = pure
