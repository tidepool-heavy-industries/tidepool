{-# LANGUAGE OverloadedStrings #-}
module JsonIntrinsic where

import qualified Data.Map.Strict as Map
import qualified Data.Text
import Tidepool.Aeson.Value

result :: Int
result =
  case ( eitherDecodeValue "{\"x\":1,\"x\":2}"
       , eitherDecodeValue "["
       , eitherDecodeValue deeplyNested
       , eitherDecodeValue largeJson
       , eitherDecodeValue "{\"$serde_json::private::Number\":\"kept\"}"
       , eitherDecodeValue "{\"$serde_json::private::Number\":\"kept\",\"other\":3}"
       , eitherDecodeValue "{\"$serde_json::private::Num\\u0062er\":\"escaped\"}"
       , eitherDecodeValue "1234567890123456789012345678901234567890"
       , eitherDecodeValue "\"\\uD83D\\uDE03\""
       , eitherDecodeValue "\"\\uD83D\""
       , eitherDecodeValue "01"
       , eitherDecodeValue "true false" ) of
    ( Right (Object values), Left _, Left _, Right (Array (_ : _))
      , Right (Object reserved), Right (Object reservedMany), Right (Object escaped), Right (Number _)
      , Right (String smile), Left _, Left _, Left _ ) ->
      if Map.lookup "x" values == Just (Number (scientific 2 0))
          && Map.lookup "$serde_json::private::Number" reserved == Just (String "kept")
          && Map.lookup "other" reservedMany == Just (Number (scientific 3 0))
          && Map.lookup "$serde_json::private::Number" escaped == Just (String "escaped")
          && smile == Data.Text.pack "😃"
          && encodeValue encoded == "{\"a\":[\"snowman ☃\",true],\"z\":1.25}"
          && encodeValue (Number (scientific 10 maxBound)) == "10e9223372036854775807"
          && encodeValue (Number (scientific 0 minBound)) == "0"
          && eitherDecodeValue (encodeValue wideArray) == Right wideArray
          && eitherDecodeValue (encodeValue wideObject) == Right wideObject
        then 1 else 0
    _ -> 0

largeJson :: Data.Text.Text
largeJson = "[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99]"

deeplyNested :: Data.Text.Text
deeplyNested = Data.Text.replicate 129 "[" <> "0" <> Data.Text.replicate 129 "]"

encoded :: Value
encoded = Object (Map.fromList
  [ ("z", Number (scientific 125 (-2)))
  , ("a", Array [String (Data.Text.pack "snowman ☃"), Bool lazyTrue])
  ])
 where
  lazyTrue = id True

-- Each field forces a nested native parse while the outer encoder retains
-- cycle-detection witnesses. The tiny-nursery runner collects repeatedly.
{-# OPAQUE lazyValue #-}
lazyValue :: Int -> Value
lazyValue n = case eitherDecodeValue ("[" <> Data.Text.pack (show n) <> ",true]") of
  Right value -> value
  Left _ -> Null

wideArray :: Value
wideArray = Array (map lazyValue [1 .. 2048])

wideObject :: Value
wideObject = Object (Map.fromList [(Data.Text.pack (show n), lazyValue n) | n <- [1 .. 512]])

cycleFailure :: Data.Text.Text
cycleFailure = encodeValue (Array values)
 where
  values = Null : values

valueCycleFailure :: Data.Text.Text
valueCycleFailure = encodeValue value
 where
  value = Array [value]

depthFailure :: Data.Text.Text
depthFailure = encodeValue (nest 130)
 where
  nest 0 = Null
  nest n = Array [nest (n - 1)]

bottomFailure :: Data.Text.Text
bottomFailure = encodeValue (Array [Null, error "JSON child bottom"])

headBottomFailure :: Data.Text.Text
headBottomFailure = encodeValue (Array (error "JSON head bottom" : divergent))
 where
  divergent = divergent

sharedValue :: Data.Text.Text
sharedValue = encodeValue (Array [shared, shared])
 where
  shared = Array [Bool True, Null]
