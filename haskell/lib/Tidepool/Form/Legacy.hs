{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | LEGACY, DE-ADVERTISED. The applicative form builder that
-- "Tidepool.Form" exported before @askUser \@T@ derived the form from the
-- answer TYPE (@plans\/self-iterating-harness\/14-generic-derived-askuser-prd.md@).
--
-- It is not auto-imported, not named in any model-facing text, and not part
-- of the advertised surface: an agent writes an ordinary ADT and
-- @askUser \@T@. This module exists only so the swap stays bisectable — the
-- successor lane (@plans\/post-restart\/checkpoint-persistence-lane.md@)
-- migrates the remaining fixtures and deletes it. Do not build on it.
--
-- A 'Form' carries BOTH how to RENDER a typed @FormSpec@ JSON (the FLAT v1
-- wire, @tidepool-harness@'s @selfharness::operator@) and how to DECODE the
-- operator's flat submission into @a@; the two halves thread a positional
-- field index (@f0@, @f1@, …) identically, so render and decode always agree
-- on each field's key without a state monad or unique-label requirement.
--
-- > form :: Form Reply
-- > form = Reply <$> enumField "Lane" [("alpha", Alpha), ("beta", Beta)]
-- >              <*> intField  "Count"
-- > r <- askUserForm form            -- :: M Reply
module Tidepool.Form.Legacy
  ( Form
  , enumField
  , intField
  , textField
  , boolField
  , askUserForm
  ) where

import Prelude
import Data.Text (Text, pack, unpack)
import Control.Lens ((^?))

import Tidepool.Effects (M, askUserRaw)
import Tidepool.Aeson.Value (Object, Value, object, (.=))
import Tidepool.Aeson.FromJSON (Result (..), (.:))
import Tidepool.Aeson.Lens (_Object)

-- | A form yielding an @a@. @formFields@ emits this form's field-spec JSON
-- objects (each keyed @f<i>@) given the next free index; @formDecode@ reads
-- them back from the flat submission object with the SAME index walk, so
-- keys line up.
data Form a = Form
  { formFields :: Int -> ([Value], Int)
  , formDecode :: Object -> Int -> (Result a, Int)
  }

instance Functor Form where
  fmap g (Form r d) = Form r (\o i -> let (ra, i') = d o i in (fmapR g ra, i'))

instance Applicative Form where
  pure x = Form (\i -> ([], i)) (\_ i -> (Success x, i))
  Form rf df <*> Form ra da =
    Form
      ( \i ->
          let (u1, i1) = rf i
              (u2, i2) = ra i1
           in (u1 ++ u2, i2)
      )
      ( \o i ->
          let (rfn, i1) = df o i
              (ran, i2) = da o i1
           in (apR rfn ran, i2)
      )

-- 'Result' helpers, kept explicit so this module needs no Functor/Applicative
-- instance for 'Result'.
fmapR :: (a -> b) -> Result a -> Result b
fmapR f (Success a) = Success (f a)
fmapR _ (Error e) = Error e

apR :: Result (a -> b) -> Result a -> Result b
apR (Success f) (Success a) = Success (f a)
apR (Error e) _ = Error e
apR _ (Error e) = Error e

-- | The submission key for field position @i@: @f0@, @f1@, …
keyName :: Int -> Text
keyName i = pack ('f' : show i)

-- | One field-spec JSON object: @{"key":"f<i>","label":<label>,"kind":<kind>}@.
fieldSpec :: Int -> Text -> Value -> Value
fieldSpec i label kind = object ["key" .= keyName i, "label" .= label, "kind" .= kind]

-- | A single-line text field. Decodes a JSON string.
textField :: Text -> Form Text
textField label =
  Form
    (\i -> ([fieldSpec i label (object ["kind" .= ("text" :: Text)])], i + 1))
    (\o i -> (o .: keyName i, i + 1))

-- | An integer field. Decodes a genuine JSON number — the operator GUI
-- submits a real number, not a string to be read-parsed.
intField :: Text -> Form Int
intField label =
  Form
    (\i -> ([fieldSpec i label (object ["kind" .= ("int" :: Text)])], i + 1))
    (\o i -> (o .: keyName i, i + 1))

-- | A boolean field. Decodes a genuine JSON bool — a v1 primitive, not a
-- yes\/no choice desugaring.
boolField :: Text -> Form Bool
boolField label =
  Form
    (\i -> ([fieldSpec i label (object ["kind" .= ("bool" :: Text)])], i + 1))
    (\o i -> (o .: keyName i, i + 1))

-- | A 1-of-N choice over @(tag, typed value)@ pairs. Each pair renders one
-- 'EnumOption' with BOTH @label@ and @tag@ set to the tag 'Text'. Decodes the
-- submitted tag string and looks it up; an unrecognized tag is a decode
-- 'Error' (and so triggers 'askUserForm'\'s re-prompt).
enumField :: Text -> [(Text, a)] -> Form a
enumField label opts =
  Form
    (\i -> ([fieldSpec i label (object ["kind" .= ("enum" :: Text), "options" .= map enumOption opts])], i + 1))
    (\o i -> (decodeEnum (o .: keyName i), i + 1))
  where
    enumOption (tag, _) = object ["label" .= tag, "tag" .= tag]
    decodeEnum (Success tag) =
      maybe (Error ("unknown option: " ++ unpack tag)) Success (lookup tag opts)
    decodeEnum (Error e) = Error e

-- | Render a 'Form' as a @FormSpec@, send it to the operator via
-- 'askUserRaw', and decode the flat @{key: scalar}@ submission into @a@. On
-- a decode failure (a missing key, a wrong-shaped value, or an unrecognized
-- enum tag) RE-PROMPTS by re-presenting the same form — no 'Either' escapes
-- this surface. (Named @askUserForm@ here, not @askUser@: the advertised
-- @askUser@ is the type-directed one in "Tidepool.Form".)
askUserForm :: Form a -> M a
askUserForm form = do
  let (fields, _) = formFields form 0
  sub <- askUserRaw (object ["fields" .= fields])
  case decodeSubmission form sub of
    Success a -> pure a
    Error _ -> askUserForm form

-- | Decode a flat submission object against a 'Form'. A submission that is
-- not itself a JSON object is a decode failure.
decodeSubmission :: Form a -> Value -> Result a
decodeSubmission form sub = case sub ^? _Object of
  Just o -> fst (formDecode form o 0)
  Nothing -> Error "submission is not an object"
