module Tidepool.Session.Lib.G1 (answer) where
import Data.Proxy (Proxy(..))
import Data.Typeable (TypeRep, typeRep)

answer :: TypeRep
answer = typeRep (Proxy :: Proxy (Int, Bool, Int))
