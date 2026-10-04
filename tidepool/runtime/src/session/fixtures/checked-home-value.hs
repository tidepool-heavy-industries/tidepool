module CheckedHomeValue where

data HomeValue = HomeValue Int

homeValue :: HomeValue
homeValue = HomeValue 41

homeNumber :: HomeValue -> Int
homeNumber (HomeValue value) = value
