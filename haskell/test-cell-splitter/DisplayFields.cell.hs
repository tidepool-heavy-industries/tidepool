{-# LANGUAGE NoOverloadedStrings, TypeOperators, RankNTypes #-}
import qualified Tidepool.Inspection as D
import qualified CellDisplayExternal as External
import qualified Data.Text as Text
import qualified GHC.Generics as G
import qualified CellDisplayReexports as R
class Generic a where unrelated :: a -> ()
class Display a where decoy :: a -> ()
data Fields a = Fields { recursive :: Maybe (Fields a), unknown :: External.Unknown, custom :: Custom, shown :: External.Shown }
data Custom = Custom
instance D.Display Custom where displayTree _ = D.TextLeaf (Text.pack "custom-wins")
data Parameter a = Parameter a
data Higher f a = Higher (f a)
data Functions = Functions (Int -> Int)
data a :+: b = a :+: b
data Poly = Poly (forall a. a -> a)
data HiddenPoly = HiddenPoly External.PolyField
data Unboxed = Unboxed External.ByteArrayField
genericSameCell = G.from (Parameter (1 :: Int))

data Authored = Authored deriving G.Generic
data Standalone = Standalone
deriving instance G.Generic Standalone
data Reexported = Reexported deriving R.Generic
instance R.Display Reexported where displayTree _ = D.TextLeaf (Text.pack "reexport-wins")
data ForeignClass = ForeignClass
instance External.Display ForeignClass where foreignDisplay _ = ()
instance External.Generic ForeignClass where foreignGeneric _ = ()
data Special a = Special a
instance D.Display (Special Int) where displayTree _ = D.TextLeaf (Text.pack "special-wins")
