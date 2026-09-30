{-# LANGUAGE ScopedTypeVariables #-}

module Tidepool.CheckedCell
  ( CheckedSignature(..), CheckedSignatureName(..)
  , captureCheckedSignature, encodeCheckedSignature
  , rewriteCheckedAnnotations
  ) where

import Codec.CBOR.Encoding (Encoding, encodeListLen, encodeString)
import Control.Monad (forM, unless)
import Data.IORef
import Data.List (find, nubBy, sortOn)
import qualified Data.Text as T
import Data.Generics (everywhereM, mkM)
import GHC
import GHC.Core.Type (tyConsOfType)
import GHC.Core.TyCon (tyConName)
import GHC.Driver.Env (lookupType, hsc_home_unit)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Iface.Env (lookupOrig)
import GHC.Iface.Load (importDecl)
import GHC.Tc.Utils.Monad (initIfaceLoad)
import qualified GHC.Data.Maybe as MErr
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence
  ( isDataOcc, mkDataOcc, mkTcOcc, occNameString )
import GHC.Types.Name.Ppr (mkNamePprCtx)
import GHC.Types.Name.Reader (RdrName(..), GlobalRdrEnv, emptyGlobalRdrEnv, rdrNameOcc)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Types (stringToUnit, unitString)
import GHC.Unit.Home (isHomeUnit)
import GHC.Utils.Outputable hiding ((<>), text)

-- The rendering is parser input. Each external type-level Name has a private
-- qualifier, independent of lexical imports or the user's pretty-print scope.
data CheckedSignature = CheckedSignature
  { signatureKey :: String
  , signatureType :: String
  , signatureNames :: [CheckedSignatureName]
  } deriving (Eq, Ord, Show)

data CheckedSignatureName = CheckedSignatureName
  { signatureQualifier :: String
  , signatureUnit :: String
  , signatureModule :: String
  , signatureNamespace :: String
  , signatureOccurrence :: String
  } deriving (Eq, Ord, Show)

captureCheckedSignature :: HscEnv -> String -> Type -> CheckedSignature
captureCheckedSignature env key ty = CheckedSignature key rendered inventory
  where
    originalNames = sortOn identity $ nubBy (==)
      [ tyConName constructor
      | constructor <- nonDetEltsUniqSet (tyConsOfType ty)
      , Just _ <- [nameModule_maybe (tyConName constructor)] ]
    identity :: Name -> (String, String, String, String)
    identity name = case nameModule_maybe name of
      Just owner -> (unitString (moduleUnit owner), moduleNameString (moduleName owner),
        namespace name, occNameString (nameOccName name))
      Nothing -> ("", "", namespace name, occNameString (nameOccName name))
    namespace :: Name -> String
    namespace name = if isDataOcc (nameOccName name) then "data" else "type"
    inventory =
      [ CheckedSignatureName ("TidepoolCheckedName" ++ show index) unit owner nameSpace occurrence
      | (index, name) <- zip [(0 :: Int)..] originalNames
      , let (unit, owner, nameSpace, occurrence) = identity name ]
    names = (mkNamePprCtx (PromTickCtx True True) (hsc_unit_env env) (emptyGlobalRdrEnv :: GlobalRdrEnv))
      { queryQualifyName = \owner occurrence ->
          case find (\entry -> signatureUnit entry == unitString (moduleUnit owner)
              && signatureModule entry == moduleNameString (moduleName owner)
              && signatureOccurrence entry == occNameString occurrence
              && signatureNamespace entry == if isDataOcc occurrence then "data" else "type") inventory of
            Just entry -> NameQual (mkModuleName (signatureQualifier entry))
            Nothing -> NameUnqual }
    rendered = renderWithContext
      (defaultSDocContext { sdocStyle = mkUserStyle names AllTheWay }) (ppr ty)

encodeCheckedSignature :: CheckedSignature -> Encoding
encodeCheckedSignature signature = encodeListLen 3
  <> text (signatureKey signature) <> text (signatureType signature)
  <> encodeListLen (fromIntegral (length (signatureNames signature)))
  <> foldMap (\entry -> encodeListLen 5
      <> text (signatureQualifier entry) <> text (signatureUnit entry)
      <> text (signatureModule entry) <> text (signatureNamespace entry)
      <> text (signatureOccurrence entry)) (signatureNames signature)
  where text = encodeString . T.pack

-- Only the compiler-generated signature binder is rewritten. Authored
-- signatures, expressions and the reader environment retain ordinary lookup.
rewriteCheckedAnnotations
  :: HscEnv -> [(String, CheckedSignature)] -> ParsedModule -> IO ParsedModule
rewriteCheckedAnnotations env annotations parsed = do
  resolved <- forM annotations $ \(binder, signature) -> do
    names <- forM (signatureNames signature) $ \entry -> do
      let owner = mkModule (stringToUnit (signatureUnit entry))
            (mkModuleName (signatureModule entry))
      occurrence <- case signatureNamespace entry of
        "type" -> pure (mkTcOcc (signatureOccurrence entry))
        "data" -> pure (mkDataOcc (signatureOccurrence entry))
        _ -> fail "checked signature has an invalid namespace"
      name <- initIfaceLoad env (lookupOrig owner occurrence)
      found <- lookupType env name
      exists <- case found of
        Just _ -> pure True
        Nothing | isHomeUnit (hsc_home_unit env) (moduleUnit owner) -> pure False
        Nothing -> initIfaceLoad env (importDecl name) >>= \loaded -> pure $ case loaded of
          MErr.Succeeded _ -> True
          MErr.Failed _ -> False
      unless exists (fail "checked signature Name is unavailable in the admitted environment")
      pure (entry, name)
    pure (binder, names)
  counts <- newIORef []
  rewritten <- everywhereM (mkM (rewriteSignature counts resolved)) (pm_parsed_source parsed)
  seen <- readIORef counts
  unless (sortOn id seen == sortOn id (map fst annotations))
    (fail "generated checked annotation is missing or duplicated")
  pure parsed { pm_parsed_source = rewritten }
  where
    rewriteSignature :: IORef [String] -> [(String, [(CheckedSignatureName, Name)])] -> Sig GhcPs -> IO (Sig GhcPs)
    rewriteSignature counts resolved signature@(TypeSig extension binders ty) =
      case [(binder,names) | (binder, names) <- resolved,
          map (occNameString . rdrNameOcc . unLoc) binders == [binder]] of
        [] -> pure signature
        [(binder,names)] -> do
          modifyIORef' counts (binder :)
          TypeSig extension binders <$> everywhereM (mkM (rewriteType names)) ty
        _ -> fail "duplicate checked annotation binder"
    rewriteSignature _ _ signature = pure signature

    rewriteType :: [(CheckedSignatureName, Name)] -> HsType GhcPs -> IO (HsType GhcPs)
    rewriteType names ty@(HsTyVar extension promotion located) = case unLoc located of
      Qual qualifier occurrence -> case
          [name | (entry, name) <- names,
            signatureQualifier entry == moduleNameString qualifier,
            signatureOccurrence entry == occNameString occurrence] of
        [name] -> pure (HsTyVar extension promotion (fmap (const (Exact name)) located))
        [] -> fail "generated checked annotation contains an unproved Name"
        _ -> fail "generated checked annotation has ambiguous Name authority"
      _ -> pure ty
    rewriteType _ ty = pure ty
