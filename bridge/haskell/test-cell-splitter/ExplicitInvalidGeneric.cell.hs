{-# LANGUAGE RankNTypes #-}
import qualified GHC.Generics as G
data ExplicitPoly = ExplicitPoly (forall a. a -> a)
deriving instance G.Generic ExplicitPoly
