{-# LANGUAGE LambdaCase #-}

-- | Binder-name extraction for Lane A (declaration accumulation).
--
-- Given a Haskell source file containing top-level declarations, parse it with
-- GHC's own parser (NO typecheck) and report the binders each declaration
-- introduces as structured 'ExportItem's. The Rust runtime calls this via the
-- @--emit-binders@ mode so it never needs a Haskell parser of its own; the
-- selective re-export logic (which names a turn redefines) is driven by these
-- GHC-sourced names.
--
-- JSON boundary (written by 'emitBinders'):
--
-- > {"items":[{"kind":"value","name":"slug"},
-- >           {"kind":"type","name":"Foo","cons":["A","B"]}]}
module Tidepool.Binders
  ( ExportItem(..)
  , extractBinders
  , renderBindersJson
  , emitBinders
  ) where

import GHC
import GHC.Hs
  ( HsDecl(..), TyClDecl(..), HsDataDefn(..), ConDecl(..)
  , hsmodDecls )
import GHC.Hs.Utils (collectHsBindBinders, CollectFlag(..))
import GHC.Driver.Session (importPaths)
import GHC.Types.Name.Reader (RdrName, rdrNameOcc)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.SrcLoc (unLoc)
import Data.Foldable (toList)
import Data.List (intercalate)
import System.Environment (lookupEnv)
import System.Process (readProcess)

-- | A binder a declaration introduces.
--
-- 'EValue' is a function/value binder. 'EType' is a type/class/data head with
-- its data constructor children, so it can render as @Foo(..)@ for both export
-- and @hiding@ (a reshape hides the old type and all its constructors at once).
data ExportItem
  = EValue String
  | EType String [String]
  deriving (Eq, Show)

-- | Parse @path@ (with @includes@ on the search path) and collect the binders
-- of its top-level declarations. Parse-only: never typechecks, so a declaration
-- that references not-yet-defined names still yields its binders.
extractBinders :: FilePath -> [FilePath] -> IO [ExportItem]
extractBinders path includes = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags dflags { importPaths = importPaths dflags ++ includes }
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    _ <- depanal [] False
    graph <- getModuleGraph
    case mgModSummaries graph of
      [] -> pure []
      summaries -> do
        let isOurs ms =
              moduleNameString (moduleName (ms_mod ms)) == "SessionDecls"
            chosen = case filter isOurs summaries of
                       (s:_) -> s
                       []    -> head summaries
        pm <- parseModule chosen
        let decls = hsmodDecls (unLoc (pm_parsed_source pm))
        pure (concatMap declItems decls)

-- | The binders one top-level declaration introduces.
declItems :: LHsDecl GhcPs -> [ExportItem]
declItems ldecl = case unLoc ldecl of
  ValD _ bind -> map (EValue . occStr) (collectHsBindBinders CollNoDictBinders bind)
  TyClD _ tcd -> tyClItems tcd
  _           -> []

tyClItems :: TyClDecl GhcPs -> [ExportItem]
tyClItems = \case
  DataDecl { tcdLName = n, tcdDataDefn = defn } ->
    [ EType (occStr (unLoc n)) (conNames defn) ]
  SynDecl  { tcdLName = n } -> [ EType (occStr (unLoc n)) [] ]
  ClassDecl{ tcdLName = n } -> [ EType (occStr (unLoc n)) [] ]
  FamDecl  { tcdFam = FamilyDecl { fdLName = n } } -> [ EType (occStr (unLoc n)) [] ]
  _ -> []

-- | Data constructor names of a data/newtype definition.
conNames :: HsDataDefn GhcPs -> [String]
conNames defn = concatMap (conDeclNames . unLoc) (toList (dd_cons defn))

conDeclNames :: ConDecl GhcPs -> [String]
conDeclNames = \case
  ConDeclH98  { con_name  = n  } -> [ occStr (unLoc n) ]
  ConDeclGADT { con_names = ns } -> map (occStr . unLoc) (toList ns)

occStr :: RdrName -> String
occStr = occNameString . rdrNameOcc

-- | Extract binders from @path@ and write the JSON contract to @out@.
emitBinders :: FilePath -> [FilePath] -> FilePath -> IO ()
emitBinders path includes out = do
  items <- extractBinders path includes
  writeFile out (renderBindersJson items)

renderBindersJson :: [ExportItem] -> String
renderBindersJson items =
  "{\"items\":[" ++ intercalate "," (map renderItem items) ++ "]}"

renderItem :: ExportItem -> String
renderItem (EValue n) =
  "{\"kind\":\"value\",\"name\":" ++ jstr n ++ "}"
renderItem (EType n cons) =
  "{\"kind\":\"type\",\"name\":" ++ jstr n
    ++ ",\"cons\":[" ++ intercalate "," (map jstr cons) ++ "]}"

-- | Minimal JSON string escaping (identifiers + operator symbols only need
-- quote/backslash escaping).
jstr :: String -> String
jstr s = '"' : concatMap esc s ++ "\""
  where
    esc '"'  = "\\\""
    esc '\\' = "\\\\"
    esc c    = [c]

getLibdir :: IO FilePath
getLibdir = do
  envDir <- lookupEnv "TIDEPOOL_GHC_LIBDIR"
  case envDir of
    Just dir -> pure dir
    Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse
