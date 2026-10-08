{-# LANGUAGE DataKinds #-}
{-# LANGUAGE PolyKinds #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}
-- | Unsupported generic field diagnostics and finite-derivation cycle guard.
module Tidepool.Form.Check (SelKey, FieldCheck, Occurs, RecursiveFieldError) where
import Prelude (Bool(..), Char, Maybe(..))
import Data.Kind (Constraint, Type)
import Data.Map.Strict (Map)
import GHC.Generics (Meta(..))
import GHC.TypeLits (ErrorMessage(..), Symbol, TypeError)

type family SelKey (s :: Meta) :: Symbol where
  SelKey ('MetaSel ('Just name) su ss ds) = name
  SelKey ('MetaSel 'Nothing su ss ds) = "<positional field>"
type family FieldCheck (name :: Symbol) (a :: Type) :: Constraint where
  FieldCheck name [Char] = TypeError ('Text "`" ':<>: 'Text name ':<>: 'Text " :: String` is not supported; use Text.")
  FieldCheck name [a] = TypeError ('Text "Cannot derive a repeated-field editor for list field `" ':<>: 'Text name ':<>: 'Text "`.")
  FieldCheck name (Map k v) = TypeError ('Text "Cannot derive a repeated-field editor for Map field `" ':<>: 'Text name ':<>: 'Text "`.")
  FieldCheck name (a -> b) = TypeError ('Text "Cannot derive an input for function field `" ':<>: 'Text name ':<>: 'Text "`; use a runtime choice of original functions.")
  FieldCheck name a = ()
-- Dispatch on the Boolean before descending so the selected refusing instance
-- never asks GHC to resolve another recursive form dictionary.
type family Occurs (a :: Type) (seen :: [Type]) :: Bool where
  Occurs a '[] = 'False
  Occurs a (a ': rest) = 'True
  Occurs a (b ': rest) = Occurs a rest
type RecursiveFieldError (name :: Symbol) = TypeError
  ('Text "Cannot derive a finite form for recursive field `" ':<>: 'Text name ':<>: 'Text "`.")
