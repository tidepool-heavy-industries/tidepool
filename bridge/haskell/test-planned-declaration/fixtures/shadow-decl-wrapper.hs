{-# LANGUAGE ExtendedDefaultRules, PackageImports #-}
{-# LANGUAGE GADTs, TypeFamilies, FlexibleInstances, StandaloneDeriving, DeriveGeneric, UndecidableInstances #-}
module SessionDecls where
import Prelude
import Data.Text (Text)
import qualified "text" Data.Text as OriginalText
import Tidepool.Session.Lib.G6
import Foreign as Selected (Box(..))
{{COMPILER_DEFAULTS}}
{{TURN}}
__result :: ()
__result = ()
