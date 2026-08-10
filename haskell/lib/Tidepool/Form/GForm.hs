{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PolyKinds #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | The operator-form interpreter: derive a 'FormShape' from a type's
-- @GHC.Generics@ representation without a value of that type.
--
-- > data Destination = LocalHost | Ssh { host :: Text, port :: Int }
-- >   deriving (Generic, FromJSON)
-- >
-- > formShape @Destination
--
-- An author writes @deriving (Generic, FromJSON)@ and nothing else — no
-- instance of anything in this module, and no second description of the
-- shape.
--
-- This module derives PRESENTATION METADATA only. The submitted value comes
-- back as ordinary JSON and is decoded by the authoritative generic
-- 'Tidepool.Aeson.FromJSON.FromJSON' path — there is no second answer
-- language. What keeps the form and the decode in agreement is that both
-- walk the same @GHC.Generics@ metadata: the collector renders record
-- objects, tagged record sums, bare strings for all-nullary sums, and
-- @null@\/omission for optionals — exactly the shapes the generic decode
-- accepts.
--
-- This interpreter is deliberately narrow. It answers ONE question — what can
-- a human be shown as a form — so lists, maps, positional payload fields,
-- and recursion are rejected here at compile time. They are legal elsewhere:
-- checkpoint persistence and answer synopses run their own interpreters over
-- the same @GHC.Generics@ metadata with their own supported sets. Do not
-- collapse them into a shared codec whose supported set is the intersection.
--
-- The supported field types are 'Text', 'Int', 'Double', 'Bool', @()@,
-- @Maybe a@, and any type with a @Generic@ instance whose fields are
-- themselves supported. Everything else is rejected by 'FieldCheck' or
-- 'Occurs', naming the offending field.
module Tidepool.Form.GForm
  ( -- * Deriving a form from a type
    DerivedForm
  , formShape

    -- * Field classification
  , FormKind (..)
  , FieldKind

    -- * The generic interpreter
  , GForm (..)
  , GVariants (..)
  , GBody (..)
  , GFields (..)
  , FormField (..)
  , GNested (..)
  ) where

import Prelude
import Data.Kind (Type)
import Data.Proxy (Proxy (..))
import Data.Map.Strict (Map)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics
  ( C
  , Constructor
  , D
  , Datatype
  , Generic
  , K1
  , M1 (..)
  , Meta (..)
  , R
  , Rep
  , S
  , Selector
  , U1
  , conName
  , datatypeName
  , selName
  , (:*:)
  , (:+:)
  )
import GHC.TypeLits (ErrorMessage (..), Symbol, TypeError)

import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Form.Check
  ( FieldCheck
  , NeedsDerivingGeneric
  , Occurs
  , RecursiveFieldError
  , SelKey
  )
import Tidepool.Form.Shape
  ( ConstructorKey
  , FieldShape (..)
  , FormShape (..)
  , TypeKey
  , VariantShape (..)
  )

-- | Everything @askUser \@a@ needs: @a@ has a generic representation this
-- interpreter can present, and a 'FromJSON' instance (normally the generic
-- default) that reads the submitted JSON back. The visited set starts as
-- @'[a]@, so a type that reaches itself is rejected at the field that closes
-- the cycle.
type DerivedForm a = (Generic a, GForm '[a] (Rep a), FromJSON a)

-- | The form for @a@, derived from type metadata alone — no @a@ is required,
-- or even constructible.
formShape :: forall a. DerivedForm a => FormShape
formShape = gShape @'[a] @(Rep a) Proxy

-- ---------------------------------------------------------------------------
-- Datatype level

-- | A whole datatype: its @M1 D@ metadata plus either one constructor (a
-- product) or several (a sum).
class GForm (seen :: [Type]) (f :: Type -> Type) where
  gShape :: Proxy seen -> FormShape

-- | A single-constructor datatype IS its constructor's payload — a record
-- form, with no redundant one-branch chooser.
instance
  (Datatype d, Constructor c, GBody seen g) =>
  GForm seen (M1 D d (M1 C c g))
  where
  gShape _ = gBodyShape @seen @g (typeKey @d) (constructorKey @c)

-- | A multi-constructor datatype presents a chooser over its variants.
instance
  (Datatype d, GVariants seen (a :+: b)) =>
  GForm seen (M1 D d (a :+: b))
  where
  gShape _ = SumShape ty (gVariants @seen @(a :+: b) ty)
    where
      ty = typeKey @d

-- ---------------------------------------------------------------------------
-- Alternatives

-- | The constructors of a sum, in DECLARATION order. @GHC.Generics@ builds a
-- balanced @:+:@ tree; an in-order walk of that tree recovers the source
-- order, and nothing about the tree's shape reaches a key or a position.
class GVariants (seen :: [Type]) (f :: Type -> Type) where
  gVariants :: TypeKey -> [VariantShape]

instance (GVariants seen a, GVariants seen b) => GVariants seen (a :+: b) where
  gVariants ty = gVariants @seen @a ty ++ gVariants @seen @b ty

instance (Constructor c, GBody seen g) => GVariants seen (M1 C c g) where
  gVariants ty = [VariantShape con (gBodyShape @seen @g ty con)]
    where
      con = constructorKey @c

-- ---------------------------------------------------------------------------
-- One constructor's payload

-- | What sits under one @M1 C@: nothing, one field, or several.
class GBody (seen :: [Type]) (f :: Type -> Type) where
  gBodyShape :: TypeKey -> ConstructorKey -> FormShape

-- | A constructor with no fields contributes no control.
instance GBody seen U1 where
  gBodyShape _ _ = UnitShape

instance GFields seen (M1 S s x) => GBody seen (M1 S s x) where
  gBodyShape ty con = ProductShape ty con (gFieldShapes @seen @(M1 S s x))

instance GFields seen (a :*: b) => GBody seen (a :*: b) where
  gBodyShape ty con = ProductShape ty con (gFieldShapes @seen @(a :*: b))

-- ---------------------------------------------------------------------------
-- Fields

-- | The fields of one product node, in declaration order. Every field is a
-- NAMED record selector — the selector name is both the control's key and
-- the JSON key the generic decode reads — so a positional field is rejected
-- below at compile time.
class GFields (seen :: [Type]) (f :: Type -> Type) where
  gFieldShapes :: [FieldShape]

instance (GFields seen a, GFields seen b) => GFields seen (a :*: b) where
  gFieldShapes = gFieldShapes @seen @a ++ gFieldShapes @seen @b

-- | One named field. 'FieldKind' classifies its type; the 'FormField'
-- instance for that classification is where an unsupported type is rejected,
-- and where a nested @Generic@ type re-enters this interpreter with the
-- visited set extended.
instance
  (Selector s, FormField (FieldKind t) (SelKey s) seen t) =>
  GFields seen (M1 S s (K1 R t))
  where
  gFieldShapes =
    [FieldShape (fieldKeyFor @s) (fieldShape @(FieldKind t) @(SelKey s) @seen @t Proxy)]

-- | A positional (non-record) payload field has no selector to key a control
-- or a JSON field by. Rejected where the type is defined, matching the JSON
-- boundary's rule, instead of the old form-only numeric-position keys.
instance
  {-# OVERLAPPING #-}
  TypeError
    ( 'Text "askUser cannot present a positional constructor field."
        ':$$: 'Text "Give the constructor record syntax (named fields) so each input has a key."
    ) =>
  GFields seen (M1 S ('MetaSel 'Nothing su ss ds) (K1 R t))
  where
  gFieldShapes = error "unreachable: positional form field is a compile-time TypeError"

-- | A record field's key is its exact selector name.
fieldKeyFor :: forall (s :: Meta). Selector s => Text
fieldKeyFor = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

-- ---------------------------------------------------------------------------
-- Leaves and blessed containers

-- | How a field's type is presented. Computed from the type by 'FieldKind',
-- which is what makes @Bool@ a checkbox rather than a two-constructor enum
-- and @Maybe a@ an optional control rather than a @Nothing@\/@Just@ picker:
-- their user-facing meanings outrank their generic representations.
data FormKind
  = KText
  | KInt
  | KDouble
  | KBool
  | KUnit
  | KMaybe
  | -- | Anything else with a @Generic@ instance — the recursive case.
    KGeneric
  | -- | A shape a human form cannot present. The instance for this
    -- classification carries no implementation; it exists so that
    -- 'FieldCheck' fires once, naming the field, instead of the interpreter
    -- walking into a list's or a map's internal representation and reporting
    -- from there.
    KRejected

-- | Classify a field type. Order matters: @Maybe (Maybe a)@ must precede
-- @Maybe a@, and the blessed containers must precede the @Generic@
-- fall-through or their representations would win over their meanings.
--
-- Two kinds of equation belong here and no others: a type we implement
-- SPECIALLY (a leaf, or a container whose meaning outranks its
-- representation), and a shape whose failure we can explain better than GHC
-- can. Everything else falls through to 'KGeneric', where ordinary instance
-- resolution takes over and GHC reports a missing @Generic@ in its own words.
--
-- Do not add an equation just to produce a nicer message for a type that
-- would otherwise reach GHC. An unrecognized type getting a standard GHC
-- error is the intended outcome, not a gap. This family ROUTES; it does not
-- decide what is supported. Grown into an exhaustive supported-type
-- classifier it would reimplement instance resolution here — badly, and
-- permanently out of date with the compiler that already does it.
type family FieldKind (a :: Type) :: FormKind where
  FieldKind Text = 'KText
  FieldKind Int = 'KInt
  FieldKind Double = 'KDouble
  FieldKind Bool = 'KBool
  FieldKind () = 'KUnit
  FieldKind (Maybe (Maybe a)) = 'KRejected
  FieldKind (Maybe a) = 'KMaybe
  FieldKind [a] = 'KRejected
  FieldKind (Map k v) = 'KRejected
  FieldKind (a -> b) = 'KRejected
  FieldKind a = 'KGeneric

-- | One field's own form.
class FormField (k :: FormKind) (n :: Symbol) (seen :: [Type]) (a :: Type) where
  fieldShape :: Proxy seen -> FormShape

instance FormField 'KText n seen Text where
  fieldShape _ = StringShape

instance FormField 'KInt n seen Int where
  fieldShape _ = IntShape

instance FormField 'KDouble n seen Double where
  fieldShape _ = NumberShape

instance FormField 'KBool n seen Bool where
  fieldShape _ = BoolShape

instance FormField 'KUnit n seen () where
  fieldShape _ = UnitShape

instance
  FormField (FieldKind a) n seen a =>
  FormField 'KMaybe n seen (Maybe a)
  where
  fieldShape _ = OptionalShape (fieldShape @(FieldKind a) @n @seen @a Proxy)

-- | A nested user type. Whether it may be entered is decided BEFORE the
-- constraint that would enter it exists — see 'GNested'.
instance GNested (Occurs a seen) n seen a => FormField 'KGeneric n seen a where
  fieldShape _ = nestedShape @(Occurs a seen) @n @seen @a Proxy

-- | Descend into a nested user type, or refuse to.
--
-- The 'Bool' is the guard, and it is a class parameter rather than a check in
-- a context because GHC must REDUCE it to choose between these two instances.
-- The refusing instance asks for nothing further, so a self-referential type
-- stops here instead of unrolling: no constraint demanding the next level down
-- is ever created.
class GNested (cycle :: Bool) (n :: Symbol) (seen :: [Type]) (a :: Type) where
  nestedShape :: Proxy seen -> FormShape

instance
  (NeedsDerivingGeneric n a (Rep a), Generic a, GForm (a ': seen) (Rep a)) =>
  GNested 'False n seen a
  where
  nestedShape _ = gShape @(a ': seen) @(Rep a) Proxy

instance RecursiveFieldError n => GNested 'True n seen a where
  nestedShape _ = rejected

-- | No form exists for these types, and 'FieldCheck' says so in the author's
-- own vocabulary. The method is unreachable: the context can never be
-- discharged, because every type 'FieldKind' classifies as 'KRejected' is one
-- 'FieldCheck' rejects.
instance FieldCheck n a => FormField 'KRejected n seen a where
  fieldShape _ = rejected

rejected :: a
rejected = error "Tidepool.Form.GForm: unsupported field reached at run time"

-- ---------------------------------------------------------------------------
-- Metadata and small helpers

-- | @datatypeName@ and friends inspect only the phantom @Meta@ type, but the
-- proxy handed to them is a REAL 'Proxy' constructor rather than @undefined@:
-- the tree-walking eval oracle forces it even though the JIT does not, and a
-- bottom there diverges the two engines.
typeKey :: forall (d :: Meta). Datatype d => TypeKey
typeKey = T.pack (datatypeName (M1 Proxy :: M1 D d Proxy ()))

constructorKey :: forall (c :: Meta). Constructor c => ConstructorKey
constructorKey = T.pack (conName (M1 Proxy :: M1 C c Proxy ()))
