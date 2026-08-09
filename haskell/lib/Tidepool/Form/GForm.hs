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
-- @GHC.Generics@ representation without a value of that type, and decode a
-- 'FormAnswer' back into that type.
--
-- > data Destination = LocalHost | Ssh { host :: Text, port :: Int }
-- >   deriving (Generic)
-- >
-- > formShape @Destination
-- > decodeForm @Destination (SumAnswer "Ssh" (ProductAnswer [("host", …), …]))
--
-- An author writes @deriving (Generic)@ and nothing else — no instance of
-- anything in this module, and no second description of the shape.
--
-- Both directions are ONE class hierarchy over the SAME generic structure, so
-- they cannot disagree about field order, constructor tags, or nesting: every
-- instance that emits a key is the instance that reads it back.
--
-- This interpreter is deliberately narrow. It answers ONE question — what can
-- a human be shown as a form, and what can come back — so lists, maps, and
-- recursion are rejected here at compile time. They are legal elsewhere:
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
  , decodeForm

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
  , K1 (..)
  , M1 (..)
  , Meta
  , R
  , Rep
  , S
  , Selector
  , U1 (..)
  , conName
  , datatypeName
  , selName
  , (:*:) (..)
  , (:+:) (..)
  )
import qualified GHC.Generics as G
import GHC.TypeLits (Symbol)

import Tidepool.Form.Check
  ( FieldCheck
  , NeedsDerivingGeneric
  , Occurs
  , RecursiveFieldError
  , SelKey
  )
import Tidepool.Form.Shape
  ( ConstructorKey
  , FieldKey
  , FieldShape (..)
  , FormAnswer (..)
  , FormError (..)
  , FormShape (..)
  , TypeKey
  , VariantShape (..)
  )

-- | Everything @askUser \@a@ needs: @a@ has a generic representation, and that
-- representation is one this interpreter can present and read back. The
-- visited set starts as @'[a]@, so a type that reaches itself is rejected at
-- the field that closes the cycle.
type DerivedForm a = (Generic a, GForm '[a] (Rep a))

-- | The form for @a@, derived from type metadata alone — no @a@ is required,
-- or even constructible.
formShape :: forall a. DerivedForm a => FormShape
formShape = gShape @'[a] @(Rep a) Proxy

-- | Rebuild an @a@ from what the operator submitted. Every failure is a
-- 'FormError' value: a malformed answer never throws, so a caller can
-- re-present the form instead of consuming its continuation.
decodeForm :: forall a. DerivedForm a => FormAnswer -> Either FormError a
decodeForm ans = G.to <$> gDecode @'[a] @(Rep a) ans

-- ---------------------------------------------------------------------------
-- Datatype level

-- | A whole datatype: its @M1 D@ metadata plus either one constructor (a
-- product) or several (a sum).
class GForm (seen :: [Type]) (f :: Type -> Type) where
  gShape :: Proxy seen -> FormShape
  gDecode :: FormAnswer -> Either FormError (f p)

-- | A single-constructor datatype IS its constructor's payload — a record
-- answers with a bare 'ProductAnswer', with no redundant sum wrapper around
-- the one branch that could have been chosen.
instance
  (Datatype d, Constructor c, GBody seen g) =>
  GForm seen (M1 D d (M1 C c g))
  where
  gShape _ = gBodyShape @seen @g (typeKey @d) (constructorKey @c)
  gDecode ans = (M1 . M1) <$> gBodyDecode @seen @g ans

-- | A multi-constructor datatype always answers @'SumAnswer' con payload@,
-- including when the chosen branch is nullary.
--
-- An unknown tag is reported ONCE, against this type's own variant list — not
-- accumulated per branch. Walking @:+:@ left to right and concatenating each
-- branch's failure produces N copies of the same uninformative line;
-- 'UnknownConstructor' carries the valid constructors instead, so the message
-- can say what to pick.
instance
  (Datatype d, GVariants seen (a :+: b)) =>
  GForm seen (M1 D d (a :+: b))
  where
  gShape _ = SumShape ty (gVariants @seen @(a :+: b) ty)
    where
      ty = typeKey @d
  gDecode ans = case ans of
    SumAnswer con payload -> case gVariantDecode @seen @(a :+: b) con payload of
      Just r -> M1 <$> r
      Nothing ->
        Left (UnknownConstructor ty con (map variantKey (gVariants @seen @(a :+: b) ty)))
    other -> Left (ShapeMismatch "a choice" (describe other))
    where
      ty = typeKey @d

variantKey :: VariantShape -> ConstructorKey
variantKey (VariantShape k _) = k

-- ---------------------------------------------------------------------------
-- Alternatives

-- | The constructors of a sum, in DECLARATION order. @GHC.Generics@ builds a
-- balanced @:+:@ tree; an in-order walk of that tree recovers the source
-- order, and nothing about the tree's shape reaches a key or a position.
class GVariants (seen :: [Type]) (f :: Type -> Type) where
  gVariants :: TypeKey -> [VariantShape]

  -- | 'Nothing' means "this tag names no constructor of mine" — distinct from
  -- @Just (Left …)@, which means the tag matched and its payload was bad.
  gVariantDecode :: ConstructorKey -> FormAnswer -> Maybe (Either FormError (f p))

instance (GVariants seen a, GVariants seen b) => GVariants seen (a :+: b) where
  gVariants ty = gVariants @seen @a ty ++ gVariants @seen @b ty
  gVariantDecode con payload = case gVariantDecode @seen @a con payload of
    Just r -> Just (L1 <$> r)
    Nothing -> fmap R1 <$> gVariantDecode @seen @b con payload

instance (Constructor c, GBody seen g) => GVariants seen (M1 C c g) where
  gVariants ty = [VariantShape con (gBodyShape @seen @g ty con)]
    where
      con = constructorKey @c
  gVariantDecode con payload
    | con == constructorKey @c =
        Just (mapError (InVariant con) (M1 <$> gBodyDecode @seen @g payload))
    | otherwise = Nothing

-- ---------------------------------------------------------------------------
-- One constructor's payload

-- | What sits under one @M1 C@: nothing, one field, or several.
class GBody (seen :: [Type]) (f :: Type -> Type) where
  gBodyShape :: TypeKey -> ConstructorKey -> FormShape
  gBodyDecode :: FormAnswer -> Either FormError (f p)

-- | A constructor with no fields contributes no control, and its answer is
-- 'UnitAnswer' rather than an empty product.
instance GBody seen U1 where
  gBodyShape _ _ = UnitShape
  gBodyDecode ans = case ans of
    UnitAnswer -> Right U1
    other -> Left (ShapeMismatch "no payload" (describe other))

instance GFields seen (M1 S s x) => GBody seen (M1 S s x) where
  gBodyShape = productShape @seen @(M1 S s x)
  gBodyDecode = productDecode @seen @(M1 S s x)

instance GFields seen (a :*: b) => GBody seen (a :*: b) where
  gBodyShape = productShape @seen @(a :*: b)
  gBodyDecode = productDecode @seen @(a :*: b)

productShape :: forall seen f. GFields seen f => TypeKey -> ConstructorKey -> FormShape
productShape ty con = ProductShape ty con (fst (gFieldShapes @seen @f 1))

-- | Decode one product node. The submitted keys are checked against the
-- shape's own keys FIRST — a duplicate or a key this product does not define
-- is rejected as data, before any field is read.
productDecode :: forall seen f p. GFields seen f => FormAnswer -> Either FormError (f p)
productDecode ans = case ans of
  ProductAnswer kvs -> do
    checkKeys (map fieldKey (fst (gFieldShapes @seen @f 1))) kvs
    fst (gFieldsDecode @seen @f 1 kvs)
  other -> Left (ShapeMismatch "a group of fields" (describe other))
  where
    fieldKey (FieldShape k _) = k

checkKeys :: [FieldKey] -> [(FieldKey, FormAnswer)] -> Either FormError ()
checkKeys expected kvs = go [] (map fst kvs)
  where
    go _ [] = Right ()
    go acc (k : ks)
      | k `elem` acc = Left (DuplicateField k)
      | k `notElem` expected = Left (UnexpectedField k)
      | otherwise = go (k : acc) ks

-- ---------------------------------------------------------------------------
-- Fields

-- | The fields of one product node, in declaration order.
--
-- The 'Int' threaded through both methods is the one-based position used to
-- key a POSITIONAL field (a plain constructor argument or a tuple component).
-- It starts at 1 at each product node and is threaded identically by shape
-- production and decoding, so the two always agree. It is not a form-wide
-- counter: two different products both start at @\"1\"@.
class GFields (seen :: [Type]) (f :: Type -> Type) where
  gFieldShapes :: Int -> ([FieldShape], Int)
  gFieldsDecode :: Int -> [(FieldKey, FormAnswer)] -> (Either FormError (f p), Int)

instance (GFields seen a, GFields seen b) => GFields seen (a :*: b) where
  gFieldShapes i = (xs ++ ys, i2)
    where
      (xs, i1) = gFieldShapes @seen @a i
      (ys, i2) = gFieldShapes @seen @b i1
  gFieldsDecode i kvs = ((:*:) <$> ra <*> rb, i2)
    where
      (ra, i1) = gFieldsDecode @seen @a i kvs
      (rb, i2) = gFieldsDecode @seen @b i1 kvs

-- | One field. 'FieldKind' classifies its type; the 'FormField' instance for
-- that classification is where an unsupported type is rejected, and where a
-- nested @Generic@ type re-enters this interpreter with the visited set
-- extended.
instance
  (Selector s, FormField (FieldKind t) (SelKey s) seen t) =>
  GFields seen (M1 S s (K1 R t))
  where
  gFieldShapes i =
    ([FieldShape (fieldKeyFor @s i) (fieldShape @(FieldKind t) @(SelKey s) @seen @t Proxy)], i + 1)
  gFieldsDecode i kvs = (r, i + 1)
    where
      k = fieldKeyFor @s i
      r = case lookup k kvs of
        Nothing -> Left (MissingField k)
        Just v ->
          mapError (InField k) (M1 . K1 <$> fieldDecode @(FieldKind t) @(SelKey s) @seen @t v)

-- | A record field's key is its exact selector name; a positional field's is
-- its one-based position within this product.
fieldKeyFor :: forall (s :: Meta). Selector s => Int -> FieldKey
fieldKeyFor i = if null sel then T.pack (show i) else T.pack sel
  where
    sel = selName (M1 Proxy :: M1 S s Proxy ())

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

-- | One field's own form, and the decode that reads it back.
class FormField (k :: FormKind) (n :: Symbol) (seen :: [Type]) (a :: Type) where
  fieldShape :: Proxy seen -> FormShape
  fieldDecode :: FormAnswer -> Either FormError a

instance FormField 'KText n seen Text where
  fieldShape _ = StringShape
  fieldDecode ans = case ans of
    StringAnswer t -> Right t
    other -> Left (ShapeMismatch "text" (describe other))

instance FormField 'KInt n seen Int where
  fieldShape _ = IntShape
  fieldDecode ans = case ans of
    IntAnswer i -> Right i
    other -> Left (ShapeMismatch "a whole number" (describe other))

instance FormField 'KDouble n seen Double where
  fieldShape _ = NumberShape
  fieldDecode ans = case ans of
    NumberAnswer d -> Right d
    other -> Left (ShapeMismatch "a number" (describe other))

instance FormField 'KBool n seen Bool where
  fieldShape _ = BoolShape
  fieldDecode ans = case ans of
    BoolAnswer b -> Right b
    other -> Left (ShapeMismatch "a yes/no" (describe other))

instance FormField 'KUnit n seen () where
  fieldShape _ = UnitShape
  fieldDecode ans = case ans of
    UnitAnswer -> Right ()
    other -> Left (ShapeMismatch "no payload" (describe other))

instance
  FormField (FieldKind a) n seen a =>
  FormField 'KMaybe n seen (Maybe a)
  where
  fieldShape _ = OptionalShape (fieldShape @(FieldKind a) @n @seen @a Proxy)
  fieldDecode ans = case ans of
    OptionalAnswer Nothing -> Right Nothing
    OptionalAnswer (Just v) -> Just <$> fieldDecode @(FieldKind a) @n @seen @a v
    other -> Left (ShapeMismatch "an optional value" (describe other))

-- | A nested user type. Whether it may be entered is decided BEFORE the
-- constraint that would enter it exists — see 'GNested'.
instance GNested (Occurs a seen) n seen a => FormField 'KGeneric n seen a where
  fieldShape _ = nestedShape @(Occurs a seen) @n @seen @a Proxy
  fieldDecode = nestedDecode @(Occurs a seen) @n @seen @a

-- | Descend into a nested user type, or refuse to.
--
-- The 'Bool' is the guard, and it is a class parameter rather than a check in
-- a context because GHC must REDUCE it to choose between these two instances.
-- The refusing instance asks for nothing further, so a self-referential type
-- stops here instead of unrolling: no constraint demanding the next level down
-- is ever created.
class GNested (cycle :: Bool) (n :: Symbol) (seen :: [Type]) (a :: Type) where
  nestedShape :: Proxy seen -> FormShape
  nestedDecode :: FormAnswer -> Either FormError a

instance
  (NeedsDerivingGeneric n a (Rep a), Generic a, GForm (a ': seen) (Rep a)) =>
  GNested 'False n seen a
  where
  nestedShape _ = gShape @(a ': seen) @(Rep a) Proxy
  nestedDecode ans = G.to <$> gDecode @(a ': seen) @(Rep a) ans

instance RecursiveFieldError n => GNested 'True n seen a where
  nestedShape _ = rejected
  nestedDecode _ = rejected

-- | No form exists for these types, and 'FieldCheck' says so in the author's
-- own vocabulary. The methods are unreachable: the context can never be
-- discharged, because every type 'FieldKind' classifies as 'KRejected' is one
-- 'FieldCheck' rejects.
instance FieldCheck n a => FormField 'KRejected n seen a where
  fieldShape _ = rejected
  fieldDecode _ = rejected

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

mapError :: (FormError -> FormError) -> Either FormError a -> Either FormError a
mapError f (Left e) = Left (f e)
mapError _ (Right a) = Right a

-- | Name an answer's shape for a 'ShapeMismatch', in operator vocabulary.
describe :: FormAnswer -> Text
describe ans = case ans of
  StringAnswer _ -> "text"
  IntAnswer _ -> "a whole number"
  NumberAnswer _ -> "a number"
  BoolAnswer _ -> "a yes/no"
  UnitAnswer -> "no payload"
  OptionalAnswer _ -> "an optional value"
  ProductAnswer _ -> "a group of fields"
  SumAnswer _ _ -> "a choice"
