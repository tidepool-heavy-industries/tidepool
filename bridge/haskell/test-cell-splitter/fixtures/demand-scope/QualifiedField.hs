module QualifiedField where
import Prelude
import Data.Version
root value = Data.Version.versionBranch value
helper value = value { Data.Version.versionBranch = old }
