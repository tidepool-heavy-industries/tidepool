{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
-- | A mount compiles one descriptor and its retained typed decoder together.
-- Version 1 uses globally unique field occurrences fN and option IDs oN local
-- to a field. Drafts are flat objects; inactive branches contribute no errors.
module Tidepool.Form.Wire
  ( PreparedForm, prepareForm, formDescriptor, decodeSubmission, answerView, encodeErrors ) where
import Prelude
import qualified Data.List.NonEmpty as NE
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import Tidepool.Aeson.Value
import Tidepool.Aeson.FromJSON (FromJSON, Result(..), fromJSON)
import Tidepool.Form.Algebra
import Tidepool.View.Types
import Tidepool.View.Wire (encodeView)
import Tidepool.Inspection.Tree (renderTree)

data PreparedForm a = PreparedForm Value (Value -> Either [ValidationError] a)
  (Value -> View) (Value -> [Text])
formDescriptor :: PreparedForm a -> Value
formDescriptor (PreparedForm v _ _ _) = object ["version" .= (1::Int), "root" .= v]
decodeSubmission :: PreparedForm a -> Value -> Either [ValidationError] a
decodeSubmission (PreparedForm _ decode _ _) draft@(Object _) = decode draft
decodeSubmission _ _ = Left [ValidationError Nothing "Expected an object of control values"]
answerView :: PreparedForm a -> Value -> View
answerView (PreparedForm _ _ render _) = render
encodeErrors :: [ValidationError] -> Value
encodeErrors es = toJSON [object ["field" .= errorField e, "message" .= errorMessage e] | e <- es]

prepareForm :: Form a -> PreparedForm a
prepareForm = fst . compile 0

compile :: Int -> Form a -> (PreparedForm a, Int)
compile n (Pure a) = (PreparedForm (object ["kind" .= ("pure"::Text)]) (const (Right a)) (const (Column [])) (const []),n)
compile n (Apply ff fa) =
  let (PreparedForm vf df rf kf, n') = compile n ff
      (PreparedForm va da ra ka, n'') = compile n' fa
      decode v = applyResults (df v) (da v)
  in (PreparedForm (object ["kind" .= ("group"::Text),"children" .= [vf,va]]) decode (\v -> Column [rf v,ra v]) (\v -> kf v ++ ka v),n'')
compile n (Input label control) =
  let key = fieldId n
      result = case control of
        TextControl seed -> prepareInput "text" key label seed
        IntControl seed -> prepareInput "int" key label seed
        NumberControl seed -> prepareNumber key label seed
        BoolControl seed -> prepareInput "bool" key label seed
  in (result,n+1)
compile n (Choice label options initial) =
  let key = fieldId n
      os = NE.toList options
      decode v = do value <- readField key v; i <- optionIndex key (length os) value; pure (optionValue (os !! i))
      render v = case readField key v >>= optionIndex key (length os) of
        Right i -> Caption (optionView (os !! i)) label
        Left _ -> Column []
  in (PreparedForm (selection "choice" key label os (fmap optionId initial)) decode render (const [key]),n+1)
compile n (Many label options initial) =
  let key = fieldId n
      decodeIndices v = do
        value <- readField key v
        case value of
          Array xs -> do
            indices <- traverse (optionIndex key (length options)) xs
            if length indices == length (unique indices) then Right indices
            else Left [ValidationError (Just key) "An option was selected more than once"]
          _ -> Left [ValidationError (Just key) "Expected a list of option identities"]
      decode v = do indices <- decodeIndices v; pure [optionValue o | (i,o) <- zip [0..] options, i `elem` indices]
      render v = case decodeIndices v of
        Right indices -> Caption (Column [optionView o | (i,o) <- zip [0..] options, i `elem` indices]) label
        Left _ -> Column []
  in (PreparedForm (selection "many" key label options (map optionId initial)) decode render (const [key]),n+1)
compile n (Alternatives label options initial) =
  let key = fieldId n
      (compiled,next) = compileBranches (n+1) (NE.toList options)
      descriptor = object ["kind" .= ("alternatives"::Text), "id" .= key, "label" .= label,
        "initial" .= fmap optionId initial, "options" .= [object ["id" .= optionId i, "label" .= optionLabel o,
        "presentation" .= encodeView 8192 (optionView o), "form" .= v] | (i,(o,PreparedForm v _ _ _)) <- zip [0..] compiled]]
      selected v = readField key v >>= optionIndex key (length compiled)
      decode v = do
        i <- selected v
        let (_,PreparedForm _ d _ _) = compiled !! i
        d v
      render v = case selected v of
        Right i -> let (o,PreparedForm _ _ r _) = compiled !! i in Column [Caption (optionView o) label,r v]
        Left _ -> Column []
      keys v = key : case selected v of Right i -> let (_,PreparedForm _ _ _ k) = compiled !! i in k v; Left _ -> []
  in (PreparedForm descriptor decode render keys,next)
compile n (Present view) = (PreparedForm (object ["kind" .= ("view"::Text),"presentation" .= encodeView 8192 view]) (const (Right ())) (const view) (const []),n)
compile n (Section label child) =
  let (PreparedForm v d r k,next) = compile n child
  in (PreparedForm (object ["kind" .= ("section"::Text),"title" .= label,"child" .= v]) d (\x -> Caption (r x) label) k,next)
compile n (Refine global check child) =
  let (PreparedForm v d r k,next) = compile n child
      decode x = do
        a <- d x
        case check a of
          Right b -> Right b
          Left messages -> Left [ValidationError (localField x) message | message <- messages]
      localField x = if global then Nothing else case k x of [key] -> Just key; _ -> Nothing
  in (PreparedForm v decode r k,next)

compileBranches :: Int -> [Option (Form a)] -> ([(Option (Form a),PreparedForm a)],Int)
compileBranches n [] = ([],n)
compileBranches n (o:os) = let (p,n') = compile n (optionValue o); (ps,n'') = compileBranches n' os in ((o,p):ps,n'')

applyResults :: Either [ValidationError] (a->b) -> Either [ValidationError] a -> Either [ValidationError] b
applyResults (Right f) (Right a) = Right (f a)
applyResults (Left a) (Left b) = Left (a++b)
applyResults (Left a) _ = Left a
applyResults _ (Left b) = Left b
readField :: Text -> Value -> Either [ValidationError] Value
readField key (Object fields) = maybe (Left [ValidationError (Just key) "A value is required"]) Right (Map.lookup key fields)
readField _ _ = Left [ValidationError Nothing "Expected an object of control values"]
fieldId, optionId :: Int -> Text
fieldId n = "f" <> T.pack (show n)
optionId n = "o" <> T.pack (show n)
optionIndex :: Text -> Int -> Value -> Either [ValidationError] Int
optionIndex field count (String key) = case [i | i <- [0..count-1], optionId i == key] of
  [i] -> Right i
  _ -> Left [ValidationError (Just field) "Unknown option identity"]
optionIndex field _ _ = Left [ValidationError (Just field) "Expected an option identity"]
selection :: ToJSON initial => Text -> Text -> Text -> [Option a] -> initial -> Value
selection kind key label options initial = object ["kind" .= kind, "id" .= key, "label" .= label, "initial" .= initial,
  "options" .= [object ["id" .= optionId i,"label" .= optionLabel o,"presentation" .= encodeView 8192 (optionView o)] | (i,o) <- zip [0..] options]]
optionLabel :: Option a -> Text
optionLabel o = let (rendered,_,_) = renderTree 512 (viewTree (optionView o)) in rendered
scalarText :: Value -> Text
scalarText (String t) = t
scalarText (Number n) = T.pack (show n)
scalarText (Bool True) = "True"
scalarText (Bool False) = "False"
scalarText Null = ""
scalarText _ = ""
unique :: Eq a => [a] -> [a]
unique = foldr (\x xs -> if x `elem` xs then xs else x:xs) []

prepareInput :: (ToJSON a, FromJSON a) => Text -> Text -> Text -> Maybe a -> PreparedForm a
prepareInput kind key label seed = PreparedForm
  (object ["kind" .= kind,"id" .= key,"label" .= label,"initial" .= seed])
  (\v -> do
    value <- readField key v
    case fromJSON value of
      Success a -> Right a
      Error message -> Left [ValidationError (Just key) (T.pack message)])
  (\v -> Caption (PlainText (either (const "") scalarText (readField key v))) label)
  (const [key])

prepareNumber :: Text -> Text -> Maybe Double -> PreparedForm Double
prepareNumber key label seed =
  let PreparedForm v decode render keys = prepareInput "number" key label seed
      finite draft = do
        value <- decode draft
        if isNaN value || isInfinite value then Left [ValidationError (Just key) "Number must be finite"]
        else Right value
  in PreparedForm v finite render keys
