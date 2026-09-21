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
       , eitherDecodeValue largeJson ) of
    (Right (Object values), Left _, Left _, Right (Array (_ : _))) ->
      if Map.lookup "x" values == Just (Number (scientific 2 0)) then 1 else 0
    _ -> 0

largeJson :: Data.Text.Text
largeJson = "[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99]"

deeplyNested :: Data.Text.Text
deeplyNested = Data.Text.replicate 129 "[" <> "0" <> Data.Text.replicate 129 "]"
