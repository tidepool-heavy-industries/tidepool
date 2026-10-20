{-# LANGUAGE GHC2024 #-}
module ImportedRecord where
import Prelude
import Data.Version (Version(..))
root value = versionBranch value
helper value = value { versionBranch = old }
