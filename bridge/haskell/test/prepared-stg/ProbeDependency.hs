module ProbeDependency (libraryAndLocalDependency) where

import qualified Data.List as List

-- This binding deliberately crosses both a home-module and a package-module
-- boundary.  The prepared program must retain their real identities.
libraryAndLocalDependency :: [Int] -> Int
libraryAndLocalDependency = List.foldl' (+) 0
