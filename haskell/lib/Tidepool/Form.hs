{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | Typed forms — author a form, get a typed value. A 'Form' carries BOTH how
-- to render (to the 'Ui' eDSL) and how to decode the operator's submission into
-- @a@; the two halves thread a positional field index (@f0@, @f1@, …)
-- identically, so render and decode always agree on each field's key without a
-- state monad or unique-label requirement. Compose applicatively:
--
-- > data Reply = Reply { lane :: Lane, notes :: Text }
-- > form :: Form Reply
-- > form = Reply <$> choiceField "Pick a lane" [("a", Alpha), ("b", Beta)]
-- >              <*> textField   "Notes"
-- > r <- dialogForm form            -- :: M (Either FormError Reply)
--
-- The GUI (a single multi-field form) and the result type are linked by
-- construction. Decoding is pure Haskell (this module); the harness renders the
-- widgets and returns the raw @{values, prose}@ submission over the existing
-- @dialogAsk :: Ui -> M Value@ primitive, which stays as the untyped escape
-- hatch.
module Tidepool.Form
  ( Form
  , FormError (..)
  , prose
  , code
  , textField
  , textField'
  , multilineField
  , multilineField'
  , choiceField
  , multiChoiceField
  , boolField
  , intField
  , dialogForm
  ) where

import Prelude
import Data.Text (Text, pack, unpack)
import Control.Lens ((^?))

import Tidepool.Ui (Ui, card, keyedChoice, keyedText, keyedTextInitial, keyedMultiChoice)
import qualified Tidepool.Ui as U
import Tidepool.Effects (M, dialogAsk)
import Tidepool.Aeson.Value (Object, Value)
import Tidepool.Aeson.FromJSON (Result (..), eitherDecode, (.:), (.:?), (.!=))
import Tidepool.Aeson.Lens (key, _Object)

-- | A form yielding an @a@. Build with the field constructors and '<$>'/'<*>'.
-- @formRender@ emits this form's widgets (each keyed @f<i>@) given the next free
-- index; @formDecode@ reads them back from the submission's @values@ object
-- with the SAME index walk, so keys line up.
data Form a = Form
  { formRender :: Int -> ([Ui], Int)
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

-- | Markdown display context between fields — no input, consumes NO field
-- index, so @prose "context" *> choiceField ...@ shows the prose then the
-- choice field keeps the same @f<i>@ key a bare @choiceField@ would get.
prose :: Text -> Form ()
prose t = Form (\i -> ([U.prose t], i)) (\_ i -> (Success (), i))

-- | A fenced source block — display context, same non-consuming shape as
-- 'prose': language, then source text.
code :: Text -> Text -> Form ()
code lang src = Form (\i -> ([U.code lang src], i)) (\_ i -> (Success (), i))

-- | A single-line text field.
textField :: Text -> Form Text
textField label =
  Form
    (\i -> ([keyedText (keyName i) label False], i + 1))
    (\o i -> (o .: keyName i, i + 1))

-- | A single-line text field seeded with an initial (editable) draft.
textField' :: Text -> Text -> Form Text
textField' label initial =
  Form
    (\i -> ([keyedTextInitial (keyName i) label False initial], i + 1))
    (\o i -> (o .: keyName i, i + 1))

-- | A multiline (textarea) text field.
multilineField :: Text -> Form Text
multilineField label =
  Form
    (\i -> ([keyedText (keyName i) label True], i + 1))
    (\o i -> (o .: keyName i, i + 1))

-- | A multiline (textarea) text field seeded with an initial (editable) draft.
multilineField' :: Text -> Text -> Form Text
multilineField' label initial =
  Form
    (\i -> ([keyedTextInitial (keyName i) label True initial], i + 1))
    (\o i -> (o .: keyName i, i + 1))

-- | A radio choice over @(label-key, typed value)@ pairs. Renders the keys as a
-- radio group; decodes the selected key back to its typed value. A missing
-- selection is a decode 'Error' (the field was required).
choiceField :: Text -> [(Text, a)] -> Form a
choiceField label opts =
  Form
    (\i -> ([keyedChoice (keyName i) label [(k, k) | (k, _) <- opts]], i + 1))
    (\o i -> (decodeChoice (o .: keyName i), i + 1))
  where
    decodeChoice (Success k) =
      maybe (Error "unknown option") Success (lookup k opts)
    decodeChoice (Error e) = Error e

-- | A checkbox group over @(label-key, typed value)@ pairs — the operator
-- picks a SUBSET. Renders the keys as checkboxes; decodes the checked keys
-- (an array under @values.<key>@) back to their typed values. An unknown
-- key is a decode 'Error'; no keys checked (the key absent from the
-- submission) decodes to @[]@, not an error.
multiChoiceField :: Text -> [(Text, a)] -> Form [a]
multiChoiceField label opts =
  Form
    (\i -> ([keyedMultiChoice (keyName i) label [(k, k) | (k, _) <- opts]], i + 1))
    (\o i -> (decodeMulti ((o .:? keyName i) .!= []), i + 1))
  where
    decodeMulti (Success ks) = traverse lookupOne (ks :: [Text])
    decodeMulti (Error e) = Error e
    lookupOne k = maybe (Error ("unknown option: " ++ unpack k)) Success (lookup k opts)

-- | A Yes/No radio decoding to 'Bool'.
boolField :: Text -> Form Bool
boolField label = choiceField label [("yes", True), ("no", False)]

-- | An integer field (a text input whose contents parse as an 'Int').
intField :: Text -> Form Int
intField label =
  Form
    (\i -> ([keyedText (keyName i) label False], i + 1))
    (\o i -> (decodeInt (o .: keyName i), i + 1))
  where
    decodeInt (Success t) = case eitherDecode t of
      Right n -> Success (n :: Int)
      Left _ -> Error "not an integer"
    decodeInt (Error e) = Error e

-- | A form decode failure: a field was missing or the wrong shape.
newtype FormError = FormError Text
  deriving (Eq, Show)

-- | Render a 'Form', elicit the operator's submission, and decode it into @a@.
-- 'Left' on a missing/ill-typed field — a typed, total failure like @run@/@llm@,
-- so the caller handles it as data rather than aborting.
dialogForm :: Form a -> M (Either FormError a)
dialogForm form = do
  let (widgets, _) = formRender form 0
  sub <- dialogAsk (card "" widgets)
  pure (decodeSubmission form sub)

decodeSubmission :: Form a -> Value -> Either FormError a
decodeSubmission form sub = case sub ^? key "values" . _Object of
  Just o -> case fst (formDecode form o 0) of
    Success a -> Right a
    Error e -> Left (FormError (pack e))
  Nothing -> Left (FormError (pack "submission has no values object"))
