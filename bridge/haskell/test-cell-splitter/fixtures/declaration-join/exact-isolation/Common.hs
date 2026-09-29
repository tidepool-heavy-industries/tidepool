{-# LANGUAGE FunctionalDependencies, MultiParamTypeClasses, TypeFamilies, TypeFamilyDependencies #-}
module Common (C(..), oldBool, D(..), F, J, A(..), Hidden(..)) where
class C a where c :: a -> Int
instance C Bool where c _ = 19
oldBool :: Int
oldBool = c True
class D a b | a -> b where d :: a -> b
type family F a
type family J a = result | result -> a
class A a where
  type Associated a
  data AssociatedData a
instance A Bool where
  type Associated Bool = Int
  data AssociatedData Bool = BoolData Int
data Hidden = Hidden Int
