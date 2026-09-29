{-# LANGUAGE FunctionalDependencies, MultiParamTypeClasses, TypeFamilies #-}
module Common (C(..), oldBool, D(..), F, A(..), Hidden(..)) where
class C a where c :: a -> Int
instance C Bool where c _ = 19
oldBool :: Int
oldBool = c True
class D a b | a -> b where d :: a -> b
type family F a
class A a where type Associated a
instance A Bool where type Associated Bool = Int
data Hidden = Hidden Int
