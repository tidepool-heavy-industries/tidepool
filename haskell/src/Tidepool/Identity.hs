-- | Stable identities shared by translation, metadata, and sessions.
--
-- External ids are fingerprints of canonical module/name pairs with a reserved
-- tag byte. Local ids additionally include the GHC unique assigned within the
-- canonicalized closed program.
module Tidepool.Identity
  ( varId
  , stableVarId
  , fieldParentDisamb
  , normalizeMod
  , qualifiedName
  , binderQualName
  , checkedKeyToIdx
  ) where

import Data.Bits ((.&.), (.|.), shiftL)
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe)
import Data.Text (Text)
import qualified Data.Text as T
import Data.Word (Word64)
import GHC.Core.DataCon (dataConWorkId)
import GHC.Data.FastString (unpackFS)
import GHC.Types.FieldLabel ()
import GHC.Types.Id (Id, isDataConId_maybe)
import GHC.Types.Name (Name, isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (fieldOcc_maybe, occNameString)
import GHC.Types.Unique (getKey)
import GHC.Types.Var (Var, varName, varUnique)
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Utils.Fingerprint (Fingerprint(..), fingerprintString)
import qualified Numeric

varId :: Var -> Word64
varId value = case isDataConId_maybe value of
  Just constructor -> stableVarId (varName (dataConWorkId constructor))
  Nothing
    | isExternalName (varName value) -> stableVarId (varName value)
    | otherwise -> localVarId value

localVarId :: Var -> Word64
localVarId value =
  let unique = getKey (varUnique value)
      occurrence = occNameString (nameOccName (varName value))
      Fingerprint high _ = fingerprintString (occurrence ++ "#" ++ show unique)
  in high .&. 0x00FFFFFFFFFFFFFF

-- These aliases identify re-export paths for the same runtime entity. Matching
-- is exact; every other module name is unchanged.
moduleAliasTable :: [(String, String)]
moduleAliasTable =
  [ ("Data.Text.Internal", "Data.Text")
  , ("GHC.Internal.Maybe", "GHC.Maybe")
  ]

normalizeMod :: String -> String
normalizeMod name = fromMaybe name (lookup name moduleAliasTable)

qualifiedName :: Name -> Text
qualifiedName name = case nameModule_maybe name of
  Just module_ -> T.pack
    (normalizeMod (moduleNameString (moduleName module_))
      ++ "." ++ occNameString (nameOccName name))
  Nothing -> T.pack (occNameString (nameOccName name))

binderQualName :: Id -> Text
binderQualName value = case isDataConId_maybe value of
  Just constructor -> qualifiedName (varName (dataConWorkId constructor))
  Nothing
    | isExternalName (varName value) -> qualifiedName (varName value)
    | otherwise -> T.pack
        (occNameString (nameOccName (varName value))
          ++ "#" ++ show (getKey (varUnique value)) ++ " (local)")

-- | Index stable ids while rejecting a collision between distinct names.
checkedKeyToIdx :: [(Word64, Text)] -> Map.Map Word64 Int
checkedKeyToIdx pairs = Map.map fst (foldl' step Map.empty (zip [0 :: Int ..] pairs))
  where
    step entries (index, (key, name)) = case Map.lookup key entries of
      Just (_, previous) | previous /= name ->
        error $ "varId collision: 0x" ++ Numeric.showHex key ""
          ++ " maps to both " ++ T.unpack previous ++ " and " ++ T.unpack name
      _ -> Map.insert key (index, name) entries

stableVarId :: Name -> Word64
stableVarId name = stableVarIdWith (fieldParentDisamb name) name

-- | Record fields include their parent in the fingerprint so duplicate field
-- labels from different record types remain distinct.
fieldParentDisamb :: Name -> String
fieldParentDisamb name = case fieldOcc_maybe (nameOccName name) of
  Just parent -> '@' : unpackFS parent
  Nothing -> ""

stableVarIdWith :: String -> Name -> Word64
stableVarIdWith disambiguator name =
  let moduleName_ = case nameModule_maybe name of
        Just module_ -> normalizeMod (moduleNameString (moduleName module_))
        Nothing -> "WiredIn"
      occurrence = occNameString (nameOccName name)
      Fingerprint high _ = fingerprintString
        (moduleName_ ++ ":" ++ occurrence ++ disambiguator)
  in (0xFE `shiftL` 56) .|. (high .&. 0x00FFFFFFFFFFFFFF)
