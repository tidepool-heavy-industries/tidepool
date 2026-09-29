{-# LANGUAGE FunctionalDependencies, MultiParamTypeClasses, TypeFamilies, GADTs,
  ConstraintKinds, FlexibleContexts, TypeOperators #-}
module BadFD where
import Joined
import Data.Kind (Constraint)
import Data.Type.Equality

data Dict (c :: Constraint) where Dict :: c => Dict c
bad :: Dict (D Int Bool) -> Dict (D Int Char) -> Bool :~: Char
bad Dict Dict = Refl
