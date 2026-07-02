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
    -- * Statement binders (session-eval bind-vs-expr classification)
  , StmtBinders(..)
  , extractStmtBinders
  , renderStmtBindersJson
  , emitStmtBinders
  ) where

import GHC
import GHC.Hs
  ( HsDecl(..), TyClDecl(..), HsDataDefn(..), ConDecl(..)
  , Sig(..), LSig, hsmodDecls )
import GHC.Hs.Expr (StmtLR(..))
import GHC.Hs.Utils (collectHsBindBinders, collectLStmtBinders, CollectFlag(..))
import GHC.Driver.Session (importPaths, xopt_set)
import GHC.LanguageExtensions (Extension(..))
import GHC.Parser (parseStatement)
import GHC.Parser.Lexer (ParseResult(..), unP, initParserState)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Data.FastString (mkFastString)
import GHC.Types.SrcLoc (mkRealSrcLoc)
import GHC.Types.Name.Reader (RdrName, rdrNameOcc)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.SrcLoc (unLoc)
import Data.Foldable (toList)
import Data.List (intercalate, foldl', nub)
import System.Environment (lookupEnv)
import System.Process (readProcess)

-- | A binder a declaration introduces.
--
-- 'EValue' is a function/value binder. 'EType' is a type/data head with its
-- data constructor children, so it can render as @Foo(..)@ for both export and
-- @hiding@. 'EClass' is a typeclass head with its method names, so it renders
-- as @Class(..)@ (required for instances to see the methods).
data ExportItem
  = EValue String
  | EType String [String]
  | EClass String [String]
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
  ClassDecl{ tcdLName = n, tcdSigs = sigs } -> [ EClass (occStr (unLoc n)) (classMethodNames sigs) ]
  FamDecl  { tcdFam = FamilyDecl { fdLName = n } } -> [ EType (occStr (unLoc n)) [] ]
  _ -> []

-- | Method names of a class declaration (parse-only: uses 'ClassOpSig' from
-- 'tcdSigs', not the typechecked 'classMethods').
classMethodNames :: [LSig GhcPs] -> [String]
classMethodNames sigs =
  nub [ occStr (unLoc nm)
      | lsig <- sigs
      , ClassOpSig _ _ ns _ <- [unLoc lsig]
      , nm <- ns ]

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
renderItem (EClass n methods) =
  "{\"kind\":\"class\",\"name\":" ++ jstr n
    ++ ",\"methods\":[" ++ intercalate "," (map jstr methods) ++ "]}"

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

--------------------------------------------------------------------------------
-- Statement binders — the session-eval bind-vs-expr signal (Lane VALUE)
--------------------------------------------------------------------------------

-- | The result of classifying one session-eval turn. @sbKind@ is @"bind"@ when
-- the turn statement introduces binders (@x <- e@ / @let x = e@), @"expr"@ for
-- a bare expression (@BodyStmt@). @sbBinders@ are the bound names (GHC-sourced),
-- empty for an expr turn. The Rust runtime picks the wrap template + the bind
-- path from this signal — it never parses Haskell itself.
data StmtBinders = StmtBinders
  { sbKind    :: String
  , sbBinders :: [String]
  } deriving (Eq, Show)

-- | Parse @src@ as a SINGLE do-statement with GHC's own parser (parse-only, no
-- typecheck) and classify it. @BindStmt@/@LetStmt@ → @"bind"@ with the bound
-- names ('collectLStmtBinders'); @BodyStmt@ (a bare expression) → @"expr"@ with
-- no binders. A parse failure is reported as @"expr"@ (the runtime then compiles
-- it through the bare-expression path, where GHC re-parses and reports any real
-- syntax error loudly).
extractStmtBinders :: String -> IO StmtBinders
extractStmtBinders src = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags0 <- getSessionDynFlags
    let dflags = foldl' xopt_set dflags0 stmtExtensions
        popts  = initParserOpts dflags
        loc    = mkRealSrcLoc (mkFastString "<turn>") 1 1
        buf    = stringToStringBuffer src
        pstate = initParserState popts buf loc
    pure $ case unP parseStatement pstate of
      POk _ lstmt -> classifyStmt lstmt
      PFailed _   -> StmtBinders "expr" []

-- | Classify a parsed statement: bare expression (@BodyStmt@) is an EXPR turn;
-- anything that binds (@BindStmt@, @LetStmt@, …) is a BIND turn carrying its
-- bound names. 'collectLStmtBinders' handles var, tuple and as-patterns.
classifyStmt :: LStmt GhcPs (LHsExpr GhcPs) -> StmtBinders
classifyStmt lstmt =
  let binders = map occStr (collectLStmtBinders CollNoDictBinders lstmt)
  in case unLoc lstmt of
       BodyStmt{} -> StmtBinders "expr" []
       _          -> StmtBinders "bind" binders

-- | Language extensions enabled for the parse-only statement classify. Broad
-- enough to cover the surface the eval template accepts (lambda-case, tuple
-- sections, block arguments, …) so a real turn classifies instead of failing to
-- parse and silently falling back to @"expr"@.
stmtExtensions :: [Extension]
stmtExtensions =
  [ LambdaCase, TupleSections, BlockArguments, MultiWayIf
  , OverloadedStrings, ScopedTypeVariables, TypeApplications
  , BangPatterns, ViewPatterns, OverloadedRecordDot
  -- QuasiQuotes: parse-only — a `[fmt|…|]` splice inside a bind turn must
  -- CLASSIFY as a bind (the quote is one token to the parser; nothing runs).
  -- Without it, `x <- … [fmt|…|] …` failed classification and fell through
  -- to the expression path ("parse error on input `<-'") — found live in the
  -- kata sweep, 2026-07-02.
  , QuasiQuotes
  ]

renderStmtBindersJson :: StmtBinders -> String
renderStmtBindersJson (StmtBinders kind binders) =
  "{\"kind\":" ++ jstr kind
    ++ ",\"binders\":[" ++ intercalate "," (map jstr binders) ++ "]}"

-- | Read the turn statement from @srcFile@, classify it, and write the JSON
-- contract to @out@. Mirrors 'emitBinders'.
emitStmtBinders :: FilePath -> FilePath -> IO ()
emitStmtBinders srcFile out = do
  src <- readFile srcFile
  sb  <- extractStmtBinders src
  writeFile out (renderStmtBindersJson sb)
