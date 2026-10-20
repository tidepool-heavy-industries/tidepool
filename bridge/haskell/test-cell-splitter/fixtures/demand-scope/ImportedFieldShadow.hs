{-# LANGUAGE GHC2024 #-}
module ImportedFieldShadow where
import Prelude
import Data.Version (Version(..))
root value = let versionBranch = id in versionBranch value
helper value = value { versionBranch = old }
