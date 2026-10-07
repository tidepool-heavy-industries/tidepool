{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}
-- | Generic records and finite sums produce the same applicative Form algebra
-- as handwritten forms. Constructors are retained functions, never JSON codecs.
module Tidepool.Form.GForm (AutoForm, autoForm, edit) where
import Prelude
import Data.Kind (Type)
import Data.List.NonEmpty (NonEmpty(..))
import Data.Proxy (Proxy(..))
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import GHC.Generics
import GHC.TypeLits (Symbol)
import Tidepool.Form.Algebra
import Tidepool.Form.Check (FieldCheck, Occurs, RecursiveFieldError, SelKey)
import Tidepool.View.Types (View(..))

type AutoForm a = FormRoot a
autoForm :: forall a. AutoForm a => Form a
autoForm = rootForm @a Nothing
edit :: AutoForm a => a -> Form a
edit = rootForm . Just

class FormRoot a where rootForm :: Maybe a -> Form a
instance {-# OVERLAPPING #-} FormRoot Text where rootForm = textInput "Text"
instance {-# OVERLAPPING #-} FormRoot Int where rootForm = intInput "Integer"
instance {-# OVERLAPPING #-} FormRoot Double where rootForm = numberInput "Number"
instance {-# OVERLAPPING #-} FormRoot Bool where rootForm = boolInput "Boolean"
instance {-# OVERLAPPING #-} FormRoot () where rootForm _ = pure ()
instance {-# OVERLAPPING #-} FormRoot a => FormRoot (Maybe a) where
  rootForm seed = optionalSeed seed (rootForm @a)
instance {-# OVERLAPPABLE #-} (Generic a, GForm '[a] (Rep a)) => FormRoot a where
  rootForm seed = to <$> gForm @'[a] (from <$> seed)

class GForm (seen :: [Type]) (f :: Type -> Type) where
  gForm :: Maybe (f p) -> Form (f p)
instance (Datatype d, GForm seen f) => GForm seen (M1 D d f) where
  gForm seed = section (T.pack (datatypeName (M1 Proxy :: M1 D d Proxy ())))
    (M1 <$> gForm @seen (unM1 <$> seed))
instance (Constructor c, GForm seen f) => GForm seen (M1 C c f) where
  gForm seed = section (T.pack (conName (M1 Proxy :: M1 C c Proxy ())))
    (M1 <$> gForm @seen (unM1 <$> seed))
instance GForm seen U1 where gForm _ = pure U1
instance (GForm seen a, GForm seen b) => GForm seen (a :*: b) where
  gForm seed = (:*:) <$> gForm @seen (left <$> seed) <*> gForm @seen (right <$> seed)
    where left (a :*: _) = a; right (_ :*: b) = b
instance GVariants seen (a :+: b) => GForm seen (a :+: b) where
  gForm seed =
    let variants = gVariants @seen seed
        options = fmap (\(v,f,_) -> option v f) variants
        selected = case [i | (i,(_,_,True)) <- zip [0..] (toList variants)] of i:_ -> Just i; [] -> Nothing
    in Alternatives "Constructor" options selected
instance (Selector s, FormField (FieldKind a) (SelKey s) seen a) => GForm seen (M1 S s (K1 R a)) where
  gForm seed = M1 . K1 <$> fieldForm @(FieldKind a) @(SelKey s) @seen @a
    (let name = T.pack (selName (M1 Proxy :: M1 S s Proxy ())) in if T.null name then "Value" else name)
    ((\(M1 (K1 a)) -> a) <$> seed)

class GVariants (seen :: [Type]) (f :: Type -> Type) where
  gVariants :: Maybe (f p) -> NonEmpty (View,Form (f p),Bool)
instance (GVariants seen a, GVariants seen b) => GVariants seen (a :+: b) where
  gVariants seed = fmap (\(v,f,s) -> (v,L1 <$> f,s)) (gVariants @seen (seedLeft seed))
    <> fmap (\(v,f,s) -> (v,R1 <$> f,s)) (gVariants @seen (seedRight seed))
    where
      seedLeft (Just (L1 a)) = Just a; seedLeft _ = Nothing
      seedRight (Just (R1 b)) = Just b; seedRight _ = Nothing
instance (Constructor c, GForm seen (M1 C c f)) => GVariants seen (M1 C c f) where
  gVariants seed = (PlainText (T.pack (conName (M1 Proxy :: M1 C c Proxy ()))),gForm @seen seed,maybe False (const True) seed) :| []

data FormKind = KText | KInt | KNumber | KBool | KUnit | KMaybe | KGeneric | KRejected
type family FieldKind (a :: Type) :: FormKind where
  FieldKind Text = 'KText
  FieldKind Int = 'KInt
  FieldKind Double = 'KNumber
  FieldKind Bool = 'KBool
  FieldKind () = 'KUnit
  FieldKind (Maybe a) = 'KMaybe
  FieldKind [a] = 'KRejected
  FieldKind (Map.Map k v) = 'KRejected
  FieldKind (a -> b) = 'KRejected
  FieldKind a = 'KGeneric
class FormField (kind :: FormKind) (name :: Symbol) (seen :: [Type]) a where
  fieldForm :: Text -> Maybe a -> Form a
instance FormField 'KText name seen Text where fieldForm = textInput
instance FormField 'KInt name seen Int where fieldForm = intInput
instance FormField 'KNumber name seen Double where fieldForm = numberInput
instance FormField 'KBool name seen Bool where fieldForm = boolInput
instance FormField 'KUnit name seen () where fieldForm _ _ = pure ()
instance FormField (FieldKind a) name seen a => FormField 'KMaybe name seen (Maybe a) where
  fieldForm label seed = optionalSeed seed (fieldForm @(FieldKind a) @name @seen @a label)
instance GNested (Occurs a seen) name seen a => FormField 'KGeneric name seen a where
  fieldForm label seed = section label (nestedForm @(Occurs a seen) @name @seen @a seed)
instance FieldCheck name a => FormField 'KRejected name seen a where
  fieldForm _ _ = error "unreachable: unsupported autoForm field"
class GNested (cycle :: Bool) (name :: Symbol) (seen :: [Type]) a where
  nestedForm :: Maybe a -> Form a
instance (Generic a, GForm (a ': seen) (Rep a)) => GNested 'False name seen a where
  nestedForm seed = to <$> gForm @(a ': seen) (from <$> seed)
instance RecursiveFieldError name => GNested 'True name seen a where
  nestedForm _ = error "unreachable: recursive autoForm field"

optionalSeed :: Maybe (Maybe a) -> (Maybe a -> Form a) -> Form (Maybe a)
optionalSeed seed child = Alternatives "Optional"
  (option (PlainText "Absent") (pure Nothing) :| [option (PlainText "Present") (Just <$> child inner)]) selected
  where
    inner = case seed of Just (Just a) -> Just a; _ -> Nothing
    selected = case seed of Just Nothing -> Just 0; Just (Just _) -> Just 1; Nothing -> Nothing
toList :: NonEmpty a -> [a]
toList (a :| as) = a:as
