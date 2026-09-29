{-# LANGUAGE MultiParamTypeClasses, TypeFamilies #-}
module Public () where
import Common
instance C Int where c _ = 22
instance D Int Char where d _ = 'p'
type instance F Char = Int
instance A Char where type Associated Char = Char
instance {-# OVERLAPPABLE #-} C [a] where c _ = 43
instance {-# OVERLAPPING #-} C [Int] where c _ = 44
