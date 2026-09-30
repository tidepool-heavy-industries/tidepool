{-# LANGUAGE GADTs, TypeFamilies, FlexibleInstances, StandaloneDeriving, DeriveGeneric, UndecidableInstances #-}
module SessionDecls where
import Prelude hiding (id)
import Foreign (Remaining(Keep), ForeignRecord(ForeignRecord, untouched))
import qualified Foreign (Box(..), Remaining(..), ForeignRecord(..), (<+>))
{{TURN}}
__result :: IO ()
__result = pure ()
