{-# LANGUAGE ExtendedDefaultRules #-}

module CellProbe where

data G = G deriving (Eq, Show)

cell :: IO ()
cell = do
  x <- pure Nothing
  print (x == Just G)
