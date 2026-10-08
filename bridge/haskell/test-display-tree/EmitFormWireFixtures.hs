{-# LANGUAGE OverloadedStrings #-}
-- | Test-resource producer. Descriptors and validation facts come from the
-- owning Form compiler. Official Aeson encodes the resulting Value structure;
-- this does not replace Tidepool's native runtime JSON anchors.
module Main where
import Prelude
import qualified Data.Aeson as A
import qualified Data.Aeson.Key as Key
import qualified Data.ByteString.Lazy as BL
import qualified Data.Map.Strict as Map
import qualified Data.Scientific as Scientific
import Data.Text (Text)
import Data.List.NonEmpty (NonEmpty(..))
import qualified Data.Text as T
import System.Environment (getArgs)
import qualified Tidepool.Aeson.Value as V
import Tidepool.Form.Algebra
import Tidepool.Form.GForm (edit)
import Tidepool.Form.Wire
import qualified Tidepool.View as View

boundaryFixtures :: V.Value
boundaryFixtures = V.object ["schema" V..= (1::Int), "cases" V..=
  [ fixture "empty-many" (prepareForm (choices "Actions" ([] :: [Option Int]))) (V.object ["f0" V..= V.Array []])
  , fixture "blank-number-null" (prepareForm (numberInput "Number" Nothing)) (V.object ["f0" V..= V.Null])
  , fixture "blank-number-missing" (prepareForm (numberInput "Number" Nothing)) (V.object [])
  , fixture "large-integer-seed" (prepareForm (edit (9007199254740993 :: Int))) (V.object ["f0" V..= ("9007199254740993"::Text)])
  , fixture "rich-choice" (prepareForm (choice "Route"
      (option (View.column [View.text "Quick", View.markdown "**Direct route**"]) (11::Int) :|
       [option (View.column [View.text "Careful", View.markdown "**Review first**"]) 22])))
      (V.object ["f0" V..= ("o1"::Text)])
  ]]
  where
    fixture name prepared values =
      let (accepted,errors) = case decodeSubmission prepared values of Right _ -> (True,[]); Left es -> (False,es)
      in V.object ["name" V..= (name::Text), "descriptor" V..= formDescriptor prepared,
        "values" V..= values, "accepted" V..= accepted, "errors" V..= encodeErrors errors]

-- A structure-preserving fixture conversion, including exact Scientific
-- coefficient/exponent. Production requests still use the native codec.
toAeson :: V.Value -> A.Value
toAeson V.Null = A.Null
toAeson (V.Bool b) = A.Bool b
toAeson (V.String t) = A.String t
toAeson (V.Number n) = A.Number (Scientific.scientific (V.coefficient n) (V.base10Exponent n))
toAeson (V.Array xs) = A.toJSON (map toAeson xs)
toAeson (V.Object fields) = A.object [Key.fromText k A..= toAeson value | (k,value) <- Map.toList fields]
main :: IO ()
main = do
  args <- getArgs
  case args of
    [output,sourceOid,producerSha] -> do
      let payload = V.object ["producer" V..= ("Tidepool.Form.Wire.prepareForm"::Text),
            "source_oid" V..= T.pack sourceOid,"producer_source_sha256" V..= T.pack producerSha,"fixtures" V..= boundaryFixtures]
      BL.writeFile output (A.encode (toAeson payload))
    _ -> fail "usage: EmitFormWireFixtures OUTPUT SOURCE_OID PRODUCER_SOURCE_SHA256"
