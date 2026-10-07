{-# LANGUAGE LambdaCase #-}

-- | Binder-name extraction for the turn/classify lanes.
--
-- Given Haskell source, parse it with GHC's own parser (NO typecheck) and
-- report the binders declarations introduce, or classify a single statement
-- as bind\/expr\/decl. The Rust runtime never parses Haskell itself; these
-- GHC-sourced names and verdicts are the only source.
module Tidepool.Binders
  ( ExportItem(..)
  , extractBindersNamed, extractBindersNamedGhc
  , exportItemName
  , declItems
    -- * Statement binders (session-eval bind-vs-expr classification)
  , TurnKind(..)
  , turnKindWireName
  , parseTurnKind
  , StmtBinders(..)
  , classifyWithFlags
  , classifyBlock
  , renderVerdictsJson
    -- * Notebook cell splitting
  , CellSourceSpan(..)
  , CellSourceItem(..)
  , CellSplitError(..)
  , renderCellSplitError
  , PragmaKind(..)
  , LocatedPragma(..)
  , LocatedImport(..), ImportIntent(..)
  , SourcePrologue(..)
  , DeclarationSource(..)
  , CellSourcePlan(..)
  , CellStructuralDisplayTarget(..)
  , CellGenericDeclaration(..)
  , installCellStructuralDisplayDeclarations
  , cellInferenceSegments
  , omitCellGenericDeclarations
  , omitCellStructuralDisplayDeclarations
  , declarationSourceWithTemplate
  , declarationSourceWithTemplateFlags
  , renderDeclarationForTemplate
  , splitCellWithFlags
  , CellAnalysisItem(..), CellBindingForm(..)
  , CellAnalysisSourceItem(..)
  , analyzeCellWithFlags
  , analyzeCell
  , analyzeOrderedCell, analyzeOrderedCellWithFlags
  , defaultParserDynFlags, templateParserFlags
  , renderCellCheckSource, renderCellCheckSourceWithLineOffset
  , PreparedTypedSegmentSource(..), prepareTypedSegmentSource
  , CellExpressionPlan(..), ExpressionLiftPlan(..)
  , CheckedBinderPin(..)
    -- * Turn-mode template selection (--turn)
  , TemplateSelector(..)
  , templateSelectorForVerdict
  , templateSelectorWireName
    -- * Turn-mode rich result (--turn)
  , TurnOut(..)
  , BoundBinder(..)
  , ValueTier(..)
  , HostBindingAuthority(..)
  , renderAskJson
  ) where

import GHC
import GHC.Driver.Session (xopt, xopt_set, parseDynamicFilePragma)
import GHC.Utils.Outputable (showSDocOneLine, defaultSDocContext, ppr)
import GHC.LanguageExtensions (Extension(..))
import GHC.Parser (parseStatement, parseDeclaration)
import qualified GHC.Parser (parseModule)
import qualified GHC.Parser as Parser (parseImport)
import GHC.Parser.Header (getOptions)
import GHC.Parser.Lexer
  ( PState
  , ParseResult(..)
  , Token(..)
  , initParserState
  , lexTokenStream
  , unP
  )
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Data.FastString (mkFastString, unpackFS)
import GHC.Types.PkgQual (RawPkgQual(..))
import GHC.Types.SrcLoc (mkRealSrcLoc, advanceSrcLoc, realSrcSpanStart, realSrcSpanEnd, srcLocLine)
import GHC.Types.Name.Reader (rdrNameOcc)
import GHC.Types.Name.Occurrence (occNameString, isSymOcc)
import GHC.Types.Error (errorsFound)
import Control.Exception (evaluate)
import Control.Monad.IO.Class (liftIO)
import Data.Char (isSpace)
import Data.Foldable (toList)
import Data.List (intercalate, isInfixOf, isPrefixOf, nub, partition, sort, stripPrefix)
import Data.Maybe (catMaybes, isJust, mapMaybe)
import Data.Text (Text)
import qualified Data.Text as T
import Data.Word (Word64)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.TypedSegment.Types
  ( TypedSegmentPlan, typedSegmentPlan, typedSegmentPlanDigest, GeneratedSegmentOperations(..)
  , TypedItemPlan(..), TypedItemBody(..) )
import Tidepool.CheckedCell
  ( renderCheckedTypeWitness, renderRequestTypeSignatures
  , CellExpressionPlan(..), ExpressionLiftPlan(..) )
import Tidepool.EffectSchema (NominalHead(..), SiteType(..), YieldSite(..))
import Tidepool.HostBindingAuthority (HostBindingAuthority(..))
import Tidepool.Json (jsonString)
import Tidepool.Timing (timeSection, emitPhase)
import Tidepool.TurnSource
  ( CompilerDefaultRecipe, emptyCompilerDefaultRecipe
  , captureCompilerDefaultRecipe, qualifyCompilerDefaultWithLineOffset
  , importQualifierNamespaces
  , prepareDeclarationTemplate, renderPreparedDeclaration )

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
-- of its top-level declarations as structured 'ExportItem's, selecting the
-- module summary by an EXACT match on @expectedModuleName@. Used by
-- @--turn@'s decl path: the caller controls the name of the scratch module it
-- just spliced and wrote (via 'Main.extractModuleName' on the spliced
-- source), so it can demand exactly that summary. A missing match is a
-- caller wiring bug — the module just written is not the module GHC parsed —
-- and fails loudly rather than silently returning a different module's
-- binders. Parse-only: never typechecks, so a declaration that references
-- not-yet-defined names still yields its binders.
extractBindersNamed :: FilePath -> [FilePath] -> String -> IO [ExportItem]
extractBindersNamed path includes expectedModuleName = do
  libdir <- getLibdir
  runGhc (Just libdir) (extractBindersNamedGhc path includes expectedModuleName)

-- | Parse through an existing compiler capability. The operation owner keeps
-- targets and finder additions local to this attempt.
extractBindersNamedGhc :: FilePath -> [FilePath] -> String -> Ghc [ExportItem]
extractBindersNamedGhc path includes expectedModuleName = do
  dflags <- getSessionDynFlags
  _ <- setSessionDynFlags dflags { importPaths = includes }
  target <- guessTarget path Nothing Nothing
  setTargets [target]
  _ <- depanal [] False
  graph <- getModuleGraph
  case filter isExpected (mgModSummaries graph) of
    (chosen : _) -> do
      pm <- parseModule chosen
      let decls = hsmodDecls (unLoc (pm_parsed_source pm))
      pure (concatMap declItems decls)
    [] -> liftIO (ioError (userError
            ("extractBindersNamed: no module named " ++ expectedModuleName
              ++ " in the parsed module graph")))
  where
    isExpected ms = moduleNameString (moduleName (ms_mod ms)) == expectedModuleName

-- | The head name an 'ExportItem' introduces — the binder for 'EValue', the
-- type/class head for 'EType'\/'EClass'. Used to derive a @--turn@ decl
-- verdict's binders from its harvested 'declItems' when the verdict itself
-- carries none (see 'TDecl's doc).
exportItemName :: ExportItem -> String
exportItemName (EValue n)   = n
exportItemName (EType n _)  = n
exportItemName (EClass n _) = n

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

--------------------------------------------------------------------------------
-- Statement binders — the session-eval bind-vs-expr signal
--------------------------------------------------------------------------------

-- | The three mutually-exclusive shapes a session-eval turn classifies to
-- (GHC-sourced) — the verdict's wire contract. Mirrors Rust's own 'TurnKind'
-- (@tidepool/runtime/src/session/turn.rs@) exactly; kept a plain 3-value type
-- here (not a refinement of template selection — see 'TemplateSelector' for
-- that) for the same reason Rust keeps them separate.
data TurnKind = KDecl | KBind | KExpr
  deriving (Eq, Show)

-- | The wire-name string every JSON verdict's @kind@ field and the
-- @--turn-verdict@ CLI argument both use. Mirrors Rust's
-- @turn_kind_wire_name@ byte for byte.
turnKindWireName :: TurnKind -> String
turnKindWireName KDecl = "decl"
turnKindWireName KBind = "bind"
turnKindWireName KExpr = "expr"

-- | Parse a wire-name string back into a 'TurnKind' — used by
-- @--turn-verdict@, where the Rust caller forwards its own already-classified
-- 'TurnKind' verbatim (via 'turnKindWireName'), so an unrecognized string
-- here means version skew. Fails loudly rather than silently defaulting,
-- mirroring Rust's own @parse_one_verdict@ read of the extract's classify
-- JSON, which rejects an unrecognized @kind@ the same way.
parseTurnKind :: String -> TurnKind
parseTurnKind "decl" = KDecl
parseTurnKind "bind" = KBind
parseTurnKind "expr" = KExpr
parseTurnKind s      = error ("unrecognized turn kind: " ++ s)

-- | The result of classifying one session-eval turn. @sbKind@ is 'KBind' when
-- the turn statement introduces binders (@x <- e@ / @let x = e@), 'KExpr' for
-- a bare expression (@BodyStmt@). @sbBinders@ are the bound names (GHC-sourced),
-- empty for an expr turn. The Rust runtime picks the wrap template + the bind
-- path from this signal — it never parses Haskell itself. @sbDeclItems@ are
-- the structured export facts from that same parse; signatures and non-decl
-- turns carry none.
data StmtBinders = StmtBinders
  { sbKind      :: TurnKind
  , sbBinders   :: [String]
  , sbDeclItems :: [ExportItem]
  } deriving (Eq, Show)

-- | Exact cell coordinates reported by GHC's lexer. Lines and columns are
-- one-based, matching GHC diagnostics.
data CellSourceSpan = CellSourceSpan
  { cellStartLine :: Int
  , cellStartColumn :: Int
  , cellEndLine :: Int
  , cellEndColumn :: Int
  } deriving (Eq, Show)

-- | One source item in a notebook cell. Lexical items retain complete original
-- lines. Parsed statement fragments retain their original columns and interior
-- bytes; preceding statements on the first line become column-preserving spaces.
data CellSourceItem = CellSourceItem
  { cellSourceSpan :: CellSourceSpan
  , cellSourceText :: String
  -- | The rendered operator text (e.g. @"."@, @"$"@, or @"`elem`"@) when this
  -- item's last non-comment token is an infix operator with nothing after it
  -- in the item. 'Nothing' when the item's last token is anything else,
  -- including when a trailing operator is followed by more of the same item
  -- on a later line (a legitimate multi-line operator chain).
  , cellSourceDanglingOperator :: Maybe String
  } deriving (Eq, Show)

data CellSplitError
  = CellLexFailure
  | CellStatementParseFailure CellSourceSpan
  | CellPrologueFailure CellSourceSpan String
  | CellHeaderFailure String
  | CellDanglingOperatorFailure CellSourceSpan String
  | CellUnsupportedLocalFixity CellSourceSpan
  deriving (Eq, Show)

renderCellSplitError :: CellSplitError -> String
renderCellSplitError CellLexFailure = "<cell>:1:1: GHC could not lex the notebook cell"
renderCellSplitError (CellStatementParseFailure sourceSpan) =
  "<cell>:" ++ show (cellStartLine sourceSpan) ++ ":" ++ show (cellStartColumn sourceSpan)
    ++ ": GHC could not parse the notebook statement list"
renderCellSplitError (CellPrologueFailure sourceSpan message) =
  "<cell>:" ++ show (cellStartLine sourceSpan) ++ ":" ++ show (cellStartColumn sourceSpan)
    ++ ": " ++ message
renderCellSplitError (CellDanglingOperatorFailure sourceSpan operatorText) =
  "<cell>:" ++ show (cellEndLine sourceSpan) ++ ":" ++ show (cellEndColumn sourceSpan)
    ++ ": cell ends with a dangling operator `" ++ operatorText
    ++ "`: remove it or supply its right operand"
renderCellSplitError (CellUnsupportedLocalFixity sourceSpan) =
  "<cell>:" ++ show (cellStartLine sourceSpan) ++ ":" ++ show (cellStartColumn sourceSpan)
    ++ ": local fixity cannot cross prepared item boundaries; put the operator and its fixity in an authored declaration group"
renderCellSplitError (CellHeaderFailure message) =
  "cell check template header: " ++ message

data PragmaKind = LanguagePragma | OptionsGhcPragma
  deriving (Eq, Show)

data LocatedPragma = LocatedPragma
  { locatedPragmaKind :: PragmaKind
  , locatedPragmaSpan :: CellSourceSpan
  , locatedPragmaSource :: String
  } deriving (Eq, Show)

-- Only an import parsed from the submitted prologue demands current source.
-- Imports installed by checking/program scaffolds retain their exact owners.
data ImportIntent
  = AuthoredSourceImport ModuleName RawPkgQual
  | RetainedGeneratedImport

instance Eq ImportIntent where
  AuthoredSourceImport leftOwner leftQualifier == AuthoredSourceImport rightOwner rightQualifier =
    leftOwner == rightOwner && sameQualifier leftQualifier rightQualifier
    where
      sameQualifier NoRawPkgQual NoRawPkgQual = True
      sameQualifier (RawPkgQual left) (RawPkgQual right) = left == right
      sameQualifier _ _ = False
  RetainedGeneratedImport == RetainedGeneratedImport = True
  _ == _ = False

instance Show ImportIntent where
  show (AuthoredSourceImport owner qualifier) =
    "AuthoredSourceImport " ++ moduleNameString owner ++ " "
      ++ showSDocOneLine defaultSDocContext (ppr qualifier)
  show RetainedGeneratedImport = "RetainedGeneratedImport"

data LocatedImport = LocatedImport
  { locatedImportSpan :: CellSourceSpan
  , locatedImportSource :: String
  , locatedImportIntent :: ImportIntent
  , locatedImportNamespaces :: [ModuleName]
  } deriving (Eq, Show)

data SourcePrologue = SourcePrologue
  { prologuePragmas :: [LocatedPragma]
  , prologueImports :: [LocatedImport]
  , prologueCompilerDefault :: CompilerDefaultRecipe
  } deriving (Eq, Show)

data DeclarationSource = DeclarationSource
  { declarationPrologue :: SourcePrologue
  , declarationBody :: String
  } deriving (Eq, Show)

data CellSourcePlan = CellSourcePlan
  { cellPlanPrologue :: SourcePrologue
  , cellPlanItems :: [CellAnalysisItem]
  , cellPlanStructuralDisplayTargets :: [CellStructuralDisplayTarget]
  , cellPlanStructuralDisplayAlias :: String
  , cellPlanDeclarationBase :: String
  , cellPlanGenericDeclarations :: [CellGenericDeclaration]
  , cellPlanStructuralDisplayDeclarations :: String
  } deriving (Eq, Show)

data CellGenericDeclaration = CellGenericDeclaration
  { genericDeclarationTarget :: String
  , genericDeclarationSource :: String
  } deriving (Eq, Show)

data CellStructuralDisplayTarget = CellStructuralDisplayTarget
  { structuralDisplayTargetName :: String
  , structuralDisplayTargetApplication :: String
  } deriving (Eq, Show)

-- Generated instances do not introduce authored source items or receipt ordinals.
installCellStructuralDisplayDeclarations :: String -> CellSourcePlan -> CellSourcePlan
installCellStructuralDisplayDeclarations generated plan = plan
  { cellPlanItems = map replaceDeclaration (cellPlanItems plan)
  , cellPlanStructuralDisplayDeclarations = generated }
  where
    replaceDeclaration item
      | sbKind (cellAnalysisVerdict item) == KDecl = item
          { cellAnalysisSource = declarationItemSource item plan ++ generated }
      | otherwise = item

    declarationItemSource item current =
      let sourceItems = cellAnalysisSourceItems item
          ownDeclarations = case
            [ authored | authored <- cellPlanItems current
                       , sbKind (cellAnalysisVerdict authored) == KDecl ] of
            [_] -> cellPlanDeclarationBase current
            _ -> concat
              [ cellAnalysisSource authored
              | authored <- cellPlanItems current
              , sbKind (cellAnalysisVerdict authored) == KDecl
              , any (`elem` sourceItems) (cellAnalysisSourceItems authored)
              ]
          ownTypes = [name | EType name _ <- sbDeclItems (cellAnalysisVerdict item)]
          ownGeneric = concatMap genericDeclarationSource
            (filter ((`elem` ownTypes) . genericDeclarationTarget) (cellPlanGenericDeclarations current))
       in ownDeclarations ++ ownGeneric

-- | Preserve source order while separating each declaration run from each
-- executable run. Executable runs stay together so their statements receive
-- ordinary whole-do inference in one check.
cellInferenceSegments :: CellSourcePlan -> [CellSourcePlan]
cellInferenceSegments plan = map makeSegment (runs (cellPlanItems plan))
  where
    runs [] = []
    -- The parser already groups adjacent authored declarations. A prologue
    -- has its own reserved original owner and must not be combined with the
    -- next declaration: rendering each item would repeat that declaration.
    runs (item : rest)
      | isDeclaration item = (True, [item]) : runs rest
      | otherwise =
          let (same, remaining) = span (not . isDeclaration) rest
           in (False, item : same) : runs remaining
    isDeclaration = (== KDecl) . sbKind . cellAnalysisVerdict

    makeSegment (declarations, items) =
      let declarationItems = if declarations then items else []
          ownTypes = nub
            [ name
            | item <- declarationItems
            , EType name _ <- sbDeclItems (cellAnalysisVerdict item)
            ]
          ownSource = concatMap cellAnalysisSource declarationItems
          ownGeneric = filter ((`elem` ownTypes) . genericDeclarationTarget)
            (cellPlanGenericDeclarations plan)
          ownTargets = filter ((`elem` ownTypes) . structuralDisplayTargetName)
            (cellPlanStructuralDisplayTargets plan)
          generatedSource = concatMap genericDeclarationSource ownGeneric
          structuralDisplays = concatMap (structuralDisplayInstance (cellPlanStructuralDisplayAlias plan)) ownTargets
          segmentedItems = if declarations
            then map (\item -> item { cellAnalysisSource = ownSource ++ generatedSource ++ structuralDisplays }) items
            else items
       in plan
          { cellPlanItems = segmentedItems
          , cellPlanStructuralDisplayTargets = ownTargets
          , cellPlanGenericDeclarations = ownGeneric
          , cellPlanDeclarationBase = ownSource
          , cellPlanStructuralDisplayDeclarations = structuralDisplays
          }

omitCellGenericDeclarations :: [String] -> CellSourcePlan -> CellSourcePlan
omitCellGenericDeclarations targets plan =
  let retained = plan
        { cellPlanGenericDeclarations = filter ((`notElem` targets) . genericDeclarationTarget)
            (cellPlanGenericDeclarations plan)
        }
  in installCellStructuralDisplayDeclarations
       (concatMap (structuralDisplayInstance (cellPlanStructuralDisplayAlias retained)) (cellPlanStructuralDisplayTargets retained)) retained

omitCellStructuralDisplayDeclarations :: [String] -> CellSourcePlan -> CellSourcePlan
omitCellStructuralDisplayDeclarations targets plan =
  let retained = plan { cellPlanStructuralDisplayTargets = filter ((`notElem` targets) . structuralDisplayTargetName)
                         (cellPlanStructuralDisplayTargets plan) }
   in installCellStructuralDisplayDeclarations
        (concatMap (structuralDisplayInstance (cellPlanStructuralDisplayAlias retained)) (cellPlanStructuralDisplayTargets retained)) retained

structuralDisplayInstance :: String -> CellStructuralDisplayTarget -> String
structuralDisplayInstance qualifier target =
  let applied = structuralDisplayTargetApplication target
  in "\ninstance {-# OVERLAPPABLE #-} " ++ qualifier ++ ".GDisplay (" ++ qualifier ++ ".Rep "
    ++ applied ++ ") => " ++ qualifier ++ ".Display " ++ applied
    ++ " where\n  displayTree = " ++ qualifier ++ ".genericDisplayTree\n"

emptyPrologue :: SourcePrologue
emptyPrologue = SourcePrologue [] [] emptyCompilerDefaultRecipe

spanToCellSpan :: SrcSpan -> CellSourceSpan
spanToCellSpan (RealSrcSpan sourceSpan _) = CellSourceSpan
  (srcSpanStartLine sourceSpan) (srcSpanStartCol sourceSpan)
  (srcSpanEndLine sourceSpan) (srcSpanEndCol sourceSpan)
spanToCellSpan _ = CellSourceSpan 1 1 1 1

cellEffectiveFlags :: DynFlags -> String -> String -> IO (Either CellSplitError DynFlags)
cellEffectiveFlags initial template source = do
  let defaults = foldl' xopt_set initial stmtExtensions
      (templateMessages, templateOptions) =
        getOptions (initParserOpts defaults) (stringToStringBuffer template) "<cell-template>"
  if errorsFound templateMessages
    then pure (Left (CellHeaderFailure "GHC could not read template options"))
    else do
      (templateFlags, templateLeftovers, templateFlagMessages) <-
        parseDynamicFilePragma defaults templateOptions
      if errorsFound templateFlagMessages || not (null templateLeftovers)
        then pure (Left (CellHeaderFailure "GHC rejected template options"))
        else do
          let (sourceMessages, sourceOptions) =
                getOptions (initParserOpts templateFlags) (stringToStringBuffer source) "<cell>"
              optionSpan = case sourceOptions of
                L sourceSpan _ : _ -> spanToCellSpan sourceSpan
                [] -> CellSourceSpan 1 1 1 1
          if errorsFound sourceMessages
            then pure (Left (CellPrologueFailure optionSpan "GHC could not read cell options"))
            else do
              (effective, leftovers, sourceFlagMessages) <-
                parseDynamicFilePragma templateFlags sourceOptions
              if errorsFound sourceFlagMessages || not (null leftovers)
                then pure (Left (CellPrologueFailure optionSpan "GHC rejected cell options"))
                else if xopt Cpp effective || gopt Opt_Pp effective
                  || any (isPrefixOf "-pgmF" . unLoc) (templateOptions ++ sourceOptions)
                  then pure (Left (CellPrologueFailure optionSpan
                    "CPP and custom preprocessors are unsupported in notebook cells"))
                  else if any (\flag -> gopt flag effective)
                    [Opt_DeferTypeErrors, Opt_DeferTypedHoles, Opt_DeferOutOfScopeVariables]
                    then pure (Left (CellPrologueFailure optionSpan
                      "notebook cells must reject type errors, typed holes, and missing names before execution"))
                    else pure (Right effective)

collectPrologue
  :: DynFlags
  -> [CellSourceItem]
  -> Either CellSplitError (SourcePrologue, [CellAnalysisSourceItem], [CellSourceItem])
collectPrologue flags = go emptyPrologue [] False
  where
    go prologue headers _ [] = Right (prologue, reverse headers, [])
    go prologue headers seenImport items@(item : rest) =
      case firstToken item of
        Just (L sourceSpan (ITblockComment raw _))
          | not seenImport, Just kind <- pragmaKind raw ->
              let pragma = LocatedPragma kind (spanToCellSpan sourceSpan) raw
               in go prologue { prologuePragmas = prologuePragmas prologue ++ [pragma] }
                    (headerItem item (length headers) : headers) seenImport rest
          | otherwise -> finish prologue headers items
        Just (L _ ITimport) ->
          case unP Parser.parseImport
            (initParserState (initParserOpts flags)
              (stringToStringBuffer (cellSourceText item))
              (mkRealSrcLoc (mkFastString "<cell>")
                (cellStartLine (cellSourceSpan item)) 1)) of
            PFailed _ -> Left (CellPrologueFailure (cellSourceSpan item)
              "GHC could not parse this import declaration")
            POk _ parsed ->
              let imported = LocatedImport
                    (spanToCellSpan (getLocA parsed))
                    (showSDocOneLine defaultSDocContext (ppr (unLoc parsed)))
                    (AuthoredSourceImport (unLoc (ideclName (unLoc parsed)))
                      (ideclPkgQual (unLoc parsed)))
                    (importQualifierNamespaces (unLoc parsed))
               in go prologue { prologueImports = prologueImports prologue ++ [imported] }
                    (headerItem item (length headers) : headers) True rest
        _ -> finish prologue headers items

    finish prologue headers items =
      case mapMaybe latePrologueItem items of
        late : _ -> Left (CellPrologueFailure (cellSourceSpan late)
          "cell pragmas and imports must precede declarations and statements")
        [] -> Right (prologue, reverse headers, items)

    latePrologueItem item = case firstToken item of
      Just (L _ ITimport) -> Just item
      Just (L _ (ITblockComment raw _)) | isJust (pragmaKind raw) -> Just item
      _ -> Nothing

    firstToken item =
      case lexTokenStream (initParserOpts flags)
        (stringToStringBuffer (cellSourceText item))
        (mkRealSrcLoc (mkFastString "<cell>")
          (cellStartLine (cellSourceSpan item)) 1) of
        PFailed _ -> Nothing
        POk _ (token : _) -> Just token
        POk _ [] -> Nothing

    headerItem item ordinal = CellAnalysisSourceItem
      { cellAnalysisSourceOrdinal = ordinal
      , cellAnalysisSourceSpan = cellSourceSpan item
      , cellAnalysisSourceKind = KDecl
      }

    pragmaKind raw = do
      rest <- stripPrefix "{-#" raw
      case takeWhile (not . isSpace) (dropWhile isSpace rest) of
        "LANGUAGE" -> Just LanguagePragma
        "OPTIONS_GHC" -> Just OptionsGhcPragma
        _ -> Nothing

blankBeforeLine :: Int -> String -> String
blankBeforeLine firstBodyLine source =
  let offset = lineOffset source firstBodyLine
      blank char = if char == '\n' then '\n' else ' '
   in map blank (take offset source) ++ drop offset source

-- | One GHC-classified item of a notebook cell. The source and span always
-- refer to the submitted cell; generated checking scaffolds never become the
-- public coordinate system.
data CellBindingForm = ActionBinding | LetBinding | RecursiveBinding
  deriving (Eq, Show)

data CellAnalysisItem = CellAnalysisItem
  { cellAnalysisSpan :: CellSourceSpan
  , cellAnalysisSource :: String
  , cellAnalysisVerdict :: StmtBinders
  , cellAnalysisSourceItems :: [CellAnalysisSourceItem]
  , cellAnalysisPrologueOnly :: Bool
  , cellAnalysisBindingForm :: Maybe CellBindingForm
  } deriving (Eq, Show)

-- | One original source item retained beneath its execution item. Declaration
-- items are compiled as one group, but receipts still need their individual
-- ordinals and spans.
data CellAnalysisSourceItem = CellAnalysisSourceItem
  { cellAnalysisSourceOrdinal :: Int
  , cellAnalysisSourceSpan :: CellSourceSpan
  , cellAnalysisSourceKind :: TurnKind
  } deriving (Eq, Show)

-- | Diagnostic post-zonk type and nominal heads captured during whole-cell
-- checking. Native signatures supply the authority for generated annotations.
data CheckedBinderPin = CheckedBinderPin
  { checkedPinKey :: String
  , checkedPinType :: String
  , checkedPinHeads :: [NominalHead]
  } deriving (Eq, Show)

-- | Split and classify a cell in one GHC session. Classification is deliberately
-- separate from execution: callers may reject the complete cell before any
-- declaration or effect is committed.
analyzeCellWithFlags
  :: DynFlags
  -> String
  -> String
  -> IO (Either CellSplitError CellSourcePlan)
analyzeCellWithFlags = analyzeCellWithGrouping False

analyzeCellWithGrouping
  :: Bool -> DynFlags -> String -> String -> IO (Either CellSplitError CellSourcePlan)
analyzeCellWithGrouping ordered dflags template source = do
  flags <- cellEffectiveFlags dflags template source
  pure $ do
    effective <- flags
    defaults <- either (Left . CellHeaderFailure) Right (captureCompilerDefaultRecipe effective template)
    lexical <- splitCellWithFlags effective source
    (prologue, headerItems, bodyItems) <- collectPrologue effective lexical
    let firstBodyLine = case bodyItems of
          item : _ -> cellStartLine (cellSourceSpan item)
          [] -> maxBound
        bodySource = blankBeforeLine firstBodyLine source
    body <- splitCellWithFlags effective bodySource
    statements <- concat <$> traverse (refineExecutableSourceItems effective) body
    classified <- traverse (uncurry (classify effective)) (zip [length headerItems..] statements)
    -- Retained declaration imports are part of the next cell's namespace.
    let genericAlias = freshAlias "TidepoolCompilerGeneric" (template ++ source)
        displayAlias = freshAlias "TidepoolCompilerDisplay" (template ++ source)
        generated = automaticGenericDeclarations effective genericAlias classified
        grouped = groupDeclarations headerItems classified ""
        targets = filter ((`elem` map genericDeclarationTarget generated) . structuralDisplayTargetName)
          (structuralDisplayTargets effective classified)
        generatedImports =
          [ LocatedImport (CellSourceSpan 1 1 1 1) ("import qualified GHC.Generics as " ++ genericAlias) RetainedGeneratedImport
              [mkModuleName "GHC.Generics", mkModuleName genericAlias]
          | not (null generated) ] ++
          [ LocatedImport (CellSourceSpan 1 1 1 1) ("import qualified Tidepool.Inspection.Display as " ++ displayAlias) RetainedGeneratedImport
              [mkModuleName "Tidepool.Inspection.Display", mkModuleName displayAlias]
          | not (null targets) ]
        plan = CellSourcePlan
          { cellPlanPrologue = prologue
              { prologueImports = prologueImports prologue ++ generatedImports
              , prologueCompilerDefault = defaults }
          , cellPlanItems = grouped
          , cellPlanStructuralDisplayTargets = targets
          , cellPlanStructuralDisplayAlias = displayAlias
          , cellPlanDeclarationBase = concat
              [ cellAnalysisSource item | item <- grouped, sbKind (cellAnalysisVerdict item) == KDecl ]
          , cellPlanGenericDeclarations = generated
          , cellPlanStructuralDisplayDeclarations = ""
          }
    pure (if ordered then plan
      else installCellStructuralDisplayDeclarations (concatMap (structuralDisplayInstance displayAlias) targets) plan)
  where
    freshAlias candidate authoredSource
      | candidate `isInfixOf` authoredSource = freshAlias (candidate ++ "X") authoredSource
      | otherwise = candidate
    -- A KExpr item is spliced into a synthesized left section by
    -- 'renderExecutable' (wrapped as @(\x -> ...) (\n <item> \n)@), so a
    -- trailing dangling operator that would otherwise be a clear syntax
    -- error instead becomes a legal, meaningless left section — GHC then
    -- reports a confusing type error over the whole expression instead of
    -- the real problem. Reject it here, before any such wrapping, with a
    -- diagnostic that names the actual operator. Other verdicts (KBind,
    -- KDecl) are spliced without an enclosing section and are not at risk.
    classify effective ordinal (fragment, (verdict, bindingForm)) =
      let item = positionedExecutableSource fragment
       in if ordered && sbKind verdict /= KDecl && statementHasLocalFixity effective (cellSourceText item)
        then Left (CellUnsupportedLocalFixity (cellSourceSpan item))
        else case (sbKind verdict, cellSourceDanglingOperator item) of
          (KExpr, Just operatorText) ->
            Left (CellDanglingOperatorFailure (cellSourceSpan item) operatorText)
          _ -> Right CellAnalysisItem
              { cellAnalysisSpan = cellSourceSpan item
              , cellAnalysisSource = cellSourceText item
              , cellAnalysisVerdict = verdict
              , cellAnalysisSourceItems =
                  [ CellAnalysisSourceItem
                      { cellAnalysisSourceOrdinal = ordinal
                      , cellAnalysisSourceSpan = cellSourceSpan item
                      , cellAnalysisSourceKind = sbKind verdict
                      }
                  ]
              , cellAnalysisPrologueOnly = False
              , cellAnalysisBindingForm = bindingForm
              }
    groupDeclarations headerItems classified generated =
      if ordered
        then (if null headerItems then [] else [declarationGroup headerItems [] ""])
          ++ orderedRuns classified
        else case partition isDeclaration classified of
          ([], executable) | null headerItems -> executable
          (declarations, executable) -> declarationGroup headerItems declarations generated : executable
    orderedRuns [] = []
    orderedRuns remaining@(item : rest)
      | isDeclaration item =
          let (declarations, tailItems) = span isDeclaration remaining
           in declarationGroup [] declarations "" : orderedRuns tailItems
      | otherwise = item : orderedRuns rest
    isDeclaration =
      (== KDecl) . sbKind . cellAnalysisVerdict
    declarationGroup headerItems declarations generated =
      case headerItems ++ concatMap cellAnalysisSourceItems declarations of
        [] -> error "declarationGroup requires a source item"
        sourceItems@(firstSource : _) ->
          let firstSpan = cellAnalysisSourceSpan firstSource
              lastSpan' = foldl
                (\_ sourceItem -> cellAnalysisSourceSpan sourceItem)
                firstSpan sourceItems
              verdicts = map cellAnalysisVerdict declarations
           in CellAnalysisItem
            { cellAnalysisSpan = CellSourceSpan
                { cellStartLine = cellStartLine firstSpan
                , cellStartColumn = cellStartColumn firstSpan
                , cellEndLine = cellEndLine lastSpan'
                , cellEndColumn = cellEndColumn lastSpan'
                }
            , cellAnalysisSource = concatMap locatedDeclaration declarations ++ generated
            , cellAnalysisVerdict = StmtBinders
                KDecl
                (nub (concatMap sbBinders verdicts))
                (concatMap sbDeclItems verdicts)
            , cellAnalysisSourceItems = sourceItems
            , cellAnalysisPrologueOnly = not (null headerItems) && null declarations
            , cellAnalysisBindingForm = Nothing
            }
    locatedDeclaration item =
      "{-# LINE " ++ show (cellStartLine (cellAnalysisSpan item))
      ++ " \"<cell>\" #-}\n"
      ++ cellAnalysisSource item
      ++ if null (cellAnalysisSource item)
          || last (cellAnalysisSource item) == '\n'
        then ""
        else "\n"

-- The origin of the first source character differs between a complete lexical
-- line and an exact parsed statement span. Preserve that distinction until the
-- parser issues the position-preserving source consumed by both renderers.
data ExecutableSourceFragment
  = CompleteSourceLine CellSourceItem
  | ParsedStatementSpan CellSourceSpan String

positionedExecutableSource :: ExecutableSourceFragment -> CellSourceItem
positionedExecutableSource (CompleteSourceLine item) = item
positionedExecutableSource (ParsedStatementSpan sourceSpan source) = CellSourceItem
  { cellSourceSpan = sourceSpan
  , cellSourceText = replicate (cellStartColumn sourceSpan - 1) ' ' ++ source
  , cellSourceDanglingOperator = Nothing
  }

-- A physical source slice can contain several explicit do statements. GHC's
-- statement-list parser owns their boundaries, including nested layout and
-- quotations; classification consumes those same parsed statements.
refineExecutableSourceItems :: DynFlags -> CellSourceItem
  -> Either CellSplitError [(ExecutableSourceFragment, (StmtBinders, Maybe CellBindingForm))]
refineExecutableSourceItems flags item
  | sbKind (fst originalClassification) == KDecl = unchanged
  | otherwise = case unP parseStatement state of
      POk parsedState statement -> case unLoc statement of
        BodyStmt _ (L _ (HsDo _ (DoExpr Nothing) statements)) _ _ ->
          case reverse (unLoc statements) of
            terminal : reversed
              | length reversed > 1 && isSyntheticTerminal terminal ->
                  traverse (refine parsedState) (reverse reversed)
            terminal : [authored] | isSyntheticTerminal terminal ->
              Right [(CompleteSourceLine item, classifyWithFlagsExactFormUsing flags source
                (ParsedStatement parsedState authored))]
            _ -> parseFailure
        _ -> parseFailure
      PFailed _ -> parseFailure
  where
    source = cellSourceText item
    originalClassification = classifyWithFlagsExactForm flags source
    unchanged = Right [(CompleteSourceLine item, originalClassification)]
    parseFailure = case cellSourceDanglingOperator item of
      Just operatorText -> Left (CellDanglingOperatorFailure (cellSourceSpan item) operatorText)
      Nothing -> Left (CellStatementParseFailure (cellSourceSpan item))
    -- The synthetic terminal permits a final bind or let in the parser-only
    -- wrapper. It is removed from the authored statement inventory.
    wrapperPrefix = "do {\n"
    initialLocation = mkRealSrcLoc (mkFastString "<cell>") 1 1
    sourceLocation = foldl' advanceSrcLoc initialLocation wrapperPrefix
    wrapped = wrapperPrefix ++ source ++ "\n; ()\n}"
    state = initParserState (initParserOpts flags) (stringToStringBuffer wrapped)
      initialLocation
    terminalLocation = foldl' advanceSrcLoc
      sourceLocation (source ++ "\n; ")
    isSyntheticTerminal statement = case getLocA statement of
      RealSrcSpan span' _ -> realSrcSpanStart span' == terminalLocation
      _ -> False
    refine parsedState statement = case getLocA statement of
      RealSrcSpan span' _ -> do
        start <- sourceOffset (realSrcSpanStart span')
        end <- sourceOffset (realSrcSpanEnd span')
        let baseLine = cellStartLine (cellSourceSpan item) - srcLocLine sourceLocation
            authored = ParsedStatementSpan
                (CellSourceSpan
                  (baseLine + srcSpanStartLine span') (srcSpanStartCol span')
                  (baseLine + srcSpanEndLine span') (srcSpanEndCol span'))
                (take (end - start) (drop start source))
        if end <= start then Left CellLexFailure
          else Right (authored, classifyWithFlagsExactFormUsing flags
            (cellSourceText (positionedExecutableSource authored)) (ParsedStatement parsedState statement))
      _ -> Left CellLexFailure
    sourceOffset target = go 0 sourceLocation source
      where
        go offset location remaining
          | location == target = Right offset
          | otherwise = case remaining of
              character : rest -> go (offset + 1) (advanceSrcLoc location character) rest
              [] -> Left CellLexFailure

-- | Append standalone 'Generic' instances to the declaration item rather than
-- inventing source items. The original source coordinates and ordinals remain
-- the public receipt protocol; the generated declarations are compiled in both
-- the whole-cell check and the later staged declaration source.
automaticGenericDeclarations :: DynFlags -> String -> [CellAnalysisItem] -> [CellGenericDeclaration]
automaticGenericDeclarations flags _ _ | not (xopt StandaloneDeriving flags) = []
automaticGenericDeclarations flags qualifier declarations =
  case unP GHC.Parser.parseModule parserState of
    POk _ parsed ->
      let parsedDecls = hsmodDecls (unLoc parsed)
          targets = mapMaybe genericTarget parsedDecls
       in map renderTarget targets
    PFailed _ -> []
  where
    source = concatMap cellAnalysisSource (filter ((== KDecl) . sbKind . cellAnalysisVerdict) declarations)
    parserState = initParserState (initParserOpts flags)
      (stringToStringBuffer source) (mkRealSrcLoc (mkFastString "<cell>") 1 1)
    renderTarget target = CellGenericDeclaration (targetName target)
      ("deriving instance " ++ qualifier ++ ".Generic " ++ targetApplication target ++ "\n")

-- | Structural companions are deterministic. GHC resolves class identities
-- and rejects only generated duplicates, so qualified unrelated classes never
-- suppress a companion merely because their occurrence names match.
structuralDisplayTargets :: DynFlags -> [CellAnalysisItem] -> [CellStructuralDisplayTarget]
structuralDisplayTargets flags declarations = case unP GHC.Parser.parseModule parserState of
  PFailed _ -> []
  POk _ parsed ->
    [ CellStructuralDisplayTarget (targetName target) (targetApplication target)
    | target <- mapMaybe genericTarget (hsmodDecls (unLoc parsed)) ]
  where
    source = concatMap cellAnalysisSource (filter ((== KDecl) . sbKind . cellAnalysisVerdict) declarations)
    parserState = initParserState (initParserOpts flags)
      (stringToStringBuffer source) (mkRealSrcLoc (mkFastString "<cell>") 1 1)

data GenericTarget = GenericTarget
  { targetName :: String
  , targetApplication :: String
  }

genericTarget :: LHsDecl GhcPs -> Maybe GenericTarget
genericTarget declaration = case unLoc declaration of
  TyClD _ DataDecl { tcdLName = name, tcdTyVars = variables, tcdDataDefn = definition }
    | eligibleDataDefinition definition ->
        let typeName = occStr (unLoc name)
            appliedName = if isSymOcc (rdrNameOcc (unLoc name)) then "(" ++ typeName ++ ")" else typeName
            variableSource = showSDocOneLine defaultSDocContext (ppr variables)
            application = "(" ++ unwords (appliedName : words variableSource) ++ ")"
         in Just (GenericTarget typeName application)
  _ -> Nothing

eligibleDataDefinition :: HsDataDefn GhcPs -> Bool
eligibleDataDefinition HsDataDefn
  { dd_ctxt = Nothing
  , dd_cType = Nothing
  , dd_cons = constructors
  } = all eligibleConstructor (toList constructors)
eligibleDataDefinition _ = False

eligibleConstructor :: LConDecl GhcPs -> Bool
eligibleConstructor constructor = case unLoc constructor of
  ConDeclH98 { con_forall = False, con_ex_tvs = [], con_mb_cxt = Nothing } -> True
  _ -> False

analyzeCell :: String -> String -> IO (Either CellSplitError CellSourcePlan)
analyzeCell template source = do
  analyzeCellUsing False template source

analyzeOrderedCell :: String -> String -> IO (Either CellSplitError CellSourcePlan)
analyzeOrderedCell = analyzeCellUsing True

analyzeOrderedCellWithFlags :: DynFlags -> String -> String -> IO (Either CellSplitError CellSourcePlan)
analyzeOrderedCellWithFlags = analyzeCellWithGrouping True

analyzeCellUsing :: Bool -> String -> String -> IO (Either CellSplitError CellSourcePlan)
analyzeCellUsing ordered template source = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    liftIO (analyzeCellWithGrouping ordered dflags template source)

-- | Obtain the parser defaults for one request. Parse-only operations need
-- 'DynFlags', not a live compiler session; request owners can share this value
-- across several such operations.
templateParserFlags :: DynFlags -> String -> IO (Either CellSplitError DynFlags)
templateParserFlags flags template = cellEffectiveFlags flags template ""

defaultParserDynFlags :: IO DynFlags
defaultParserDynFlags = do
  libdir <- getLibdir
  runGhc (Just libdir) getSessionDynFlags

-- | Extract the same located header for a declaration turn without
-- reclassifying the already-checked declaration body.
declarationSourceWithTemplate
  :: String -> String -> IO (Either CellSplitError DeclarationSource)
declarationSourceWithTemplate template source = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    liftIO (declarationSourceWithTemplateFlags dflags template source)

declarationSourceWithTemplateFlags
  :: DynFlags -> String -> String -> IO (Either CellSplitError DeclarationSource)
declarationSourceWithTemplateFlags dflags template source = do
  flags <- cellEffectiveFlags dflags template source
  pure $ do
    effective <- flags
    defaults <- either (Left . CellHeaderFailure) Right (captureCompilerDefaultRecipe effective template)
    lexical <- splitCellWithFlags effective source
    (prologue, _, bodyItems) <- collectPrologue effective lexical
    let firstBodyLine = case bodyItems of
          item : _ -> cellStartLine (cellSourceSpan item)
          [] -> maxBound
    pure (DeclarationSource (prologue { prologueCompilerDefault = defaults }) (blankBeforeLine firstBodyLine source))

renderDeclarationForTemplate :: String -> DeclarationSource -> Either String String
renderDeclarationForTemplate template source = do
  let (beforeModule, afterModule) = break moduleHeader (lines template)
  case afterModule of
    [] -> Left "declaration template has no module header"
    _ ->
      let pragmas = concatMap ((++ "\n") . locatedPragmaSource)
            (prologuePragmas (declarationPrologue source))
          imports = concatMap ((++ "\n") . locatedImportSource)
            (prologueImports (declarationPrologue source))
          withPragmas = unlines beforeModule ++ pragmas ++ unlines afterModule
       in do
          prepared <- prepareDeclarationTemplate (prologueCompilerDefault (declarationPrologue source))
            (concatMap locatedImportNamespaces (prologueImports (declarationPrologue source))) imports withPragmas
          Right (renderPreparedDeclaration prepared (declarationBody source))
  where
    moduleHeader line = "module " `isPrefixOf` dropWhile isSpace line

-- | Fill the runtime-authored whole-cell template. The template owns imports,
-- the exact effect row, and expression admissibility; this function places
-- GHC-classified source and reserves the parsed rewrite's item names.
--
-- The two literal placeholders are intentionally the entire template
-- vocabulary. Missing or duplicate placeholders are rejected by the worker
-- entry point before compilation.
-- The parser's original statement form chooses the capture origin. Neither
-- renderer nor runtime parses Haskell again to distinguish a let from a bind.
data PreparedTypedSegmentSource = PreparedTypedSegmentSource
  { preparedTypedSegmentPlan :: TypedSegmentPlan
  , preparedTypedSegmentSource :: String
  , preparedTypedSegmentTemplate :: String
  , preparedTypedSegmentLineOffset :: Int
  , preparedTypedSegmentOperations :: GeneratedSegmentOperations
  }

prepareTypedSegmentSource :: String -> CellSourcePlan -> String
  -> [(Int, Word64, Maybe String)] -> Either String PreparedTypedSegmentSource
prepareTypedSegmentSource template sourcePlan reservation slots = do
  let items = cellPlanItems sourcePlan
      occupied = template ++ concatMap cellAnalysisSource items
      fresh candidate | candidate `isInfixOf` occupied = fresh (candidate ++ "X")
                      | otherwise = candidate
      root = fresh "__tidepool_segment_check"
      reserve role ordinal = fresh ("__tidepool_segment_" ++ role ++ "_" ++ show ordinal)
      namespaces = concatMap locatedImportNamespaces (prologueImports (cellPlanPrologue sourcePlan))
      freshQualifier candidate
        | mkModuleName candidate `elem` namespaces || candidate `isInfixOf` template =
            freshQualifier (candidate ++ "X")
        | otherwise = mkModuleName candidate
      qualifier = freshQualifier "TidepoolResume"
  if length slots /= length items || any ((== KDecl) . sbKind . cellAnalysisVerdict) items
    then Left "typed segment reservations do not cover one executable parser segment"
    else pure ()
  planned <- sequence
    [ let names = sbBinders (cellAnalysisVerdict item)
          entry = reserve "item" ordinal
       in case (sbKind (cellAnalysisVerdict item), cellAnalysisBindingForm item, observation) of
        (KBind, Just LetBinding, Nothing) -> Right (TypedItemPlan ordinal entry generation
          (LetItem (reserve "let" ordinal) names))
        (KBind, Just ActionBinding, Nothing) -> Right (TypedItemPlan ordinal entry generation
          (ActionItem (reserve "step" ordinal) (reserve "probe" ordinal) (reserve "match" ordinal) names))
        (KBind, Just RecursiveBinding, Nothing) -> Right (TypedItemPlan ordinal entry generation
          (ActionItem (reserve "step" ordinal) (reserve "probe" ordinal) (reserve "match" ordinal) names))
        (KExpr, Nothing, Just name) | not (null name), not (name `isInfixOf` occupied) ->
          Right (TypedItemPlan ordinal entry generation (ObservationItem (reserve "probe" ordinal) name))
        _ -> Left "typed segment reservation differs from its parser statement form"
    | (item, (ordinal, generation, observation)) <- zip items slots ]
  plan <- either (Left . show) Right (typedSegmentPlan reservation root planned)
  -- This exact private import is the existing generated-scaffold support edge.
  -- Its source/certificate owner is authenticated before Core extraction.
  protected <- replaceOnce "{{CELL_IMPORTS}}"
    ("import qualified Tidepool.Internal.Resume as " ++ moduleNameString qualifier ++ "\n{{CELL_IMPORTS}}") template
  (rendered, generatedLineOffset) <- renderCellSourceWithLineOffset False protected sourcePlan
  renamedRoot <- replaceOnce "__tidepool_cell_check ::" (root ++ " ::") rendered
    >>= replaceOnce "__tidepool_cell_check =" (root ++ " =")
  let sealed = renamedRoot ++ "\n-- tidepool-typed-segment-plan-v1 " ++ typedSegmentPlanDigest plan ++ "\n"
  pure (PreparedTypedSegmentSource plan sealed protected generatedLineOffset (GeneratedSegmentOperations qualifier))

renderCellCheckSource :: String -> CellSourcePlan -> Either String String
renderCellCheckSource template plan = fst <$> renderCellCheckSourceWithLineOffset template plan

renderCellCheckSourceWithLineOffset :: String -> CellSourcePlan -> Either String (String,Int)
renderCellCheckSourceWithLineOffset = renderCellSourceWithLineOffset True

renderCellSourceWithLineOffset :: Bool -> String -> CellSourcePlan -> Either String (String,Int)
renderCellSourceWithLineOffset checking template plan = do
  withPragmas <- replaceOnce "{{CELL_PRAGMAS}}" pragmas template
  withImports <- replaceOnce "{{CELL_IMPORTS}}" imports withPragmas
  (prepared, defaultLineOffset) <- qualifyCompilerDefaultWithLineOffset (prologueCompilerDefault (cellPlanPrologue plan))
    (concatMap locatedImportNamespaces (prologueImports (cellPlanPrologue plan))) withImports
  withDecls <- replaceOnce "{{CELL_DECLS}}" declarations prepared
  renderedBody <- body
  rendered <- replaceOnce "{{CELL_BODY}}" renderedBody withDecls
  pure (rendered, length (filter (== '\n') pragmas) + defaultLineOffset)
  where
    items = cellPlanItems plan
    prologue = cellPlanPrologue plan
    pragmas = concatMap ((++ "\n") . locatedPragmaSource) (prologuePragmas prologue)
    imports = concatMap ((++ "\n") . locatedImportSource) (prologueImports prologue)
    declarations = concat
      [ linePragma item ++ cellAnalysisSource item ++ trailingNewline (cellAnalysisSource item)
      | item <- items
      , sbKind (cellAnalysisVerdict item) == KDecl
      ]
    executable =
      [ (index, item)
      | (index, item) <- zip [(0 :: Int)..] items
      , sbKind (cellAnalysisVerdict item) /= KDecl
      ]
    body = case executable of
      [] -> Right "pure ()\n"
      values -> (++ "\n") . intercalate "\n; " <$> mapM renderExecutable values

    renderExecutable (_, item) | not checking =
      Right (linePragma item ++ cellAnalysisSource item ++ trailingNewline (cellAnalysisSource item))
    renderExecutable (index, item) =
      let prefix = if sbKind (cellAnalysisVerdict item) == KExpr then "" else linePragma item
      in (prefix ++) <$> case cellAnalysisVerdict item of
        StmtBinders KBind binders _ ->
          if null binders
            then Right (cellAnalysisSource item ++ trailingNewline (cellAnalysisSource item))
            else do
              let aliases = map (renderAlias index) binders
              Right
                ( cellAnalysisSource item
                ++ trailingNewline (cellAnalysisSource item)
                ++ "; let { "
                ++ intercalate "; " aliases
                ++ " }\n"
                )
        StmtBinders KExpr _ _ ->
          Right
            ( "(\\__tidepool_cell_expr_" ++ show index ++ " -> __tidepoolCellExpression __tidepool_cell_expr_" ++ show index ++ ") (\n"
            ++ linePragma item
            ++ cellAnalysisSource item
            ++ trailingNewline (cellAnalysisSource item)
            ++ ")\n"
            )
        StmtBinders KDecl _ _ -> Right ""

    renderAlias index binder =
      pinKey index binder ++ " = " ++ binder

    linePragma item =
      "{-# LINE " ++ show (cellStartLine (cellAnalysisSpan item)) ++ " \"<cell>\" #-}\n"
    pinKey index binder = "__tidepool_cell_pin_" ++ show index ++ "_" ++ binder
    trailingNewline text = if null text || last text == '\n' then "" else "\n"


replaceOnce :: String -> String -> String -> Either String String
replaceOnce needle replacement haystack =
  case breakOn needle haystack of
    Nothing -> Left ("cell check template is missing " ++ needle)
    Just (before, after)
      | needle `isInfixOf` after ->
          Left ("cell check template contains " ++ needle ++ " more than once")
      | otherwise -> Right (before ++ replacement ++ after)

breakOn :: Eq a => [a] -> [a] -> Maybe ([a],[a])
breakOn needle = go []
  where
    go _ [] = Nothing
    go prefix rest
      | needle `isPrefixOf` rest =
          Just (reverse prefix, drop (length needle) rest)
      | char : more <- rest = go (char : prefix) more
-- | Split a notebook cell at real tokens beginning in column one on a later
-- line. GHC's lexer, not a second Haskell grammar, decides which newlines are
-- inside strings, quasiquotes, comments, and pragmas.
--
-- This function only establishes lexical source items. Declaration grouping
-- and statement/expression classification remain separate GHC parser steps.
splitCellWithFlags :: DynFlags -> String -> Either CellSplitError [CellSourceItem]
splitCellWithFlags dflags0 source =
  case lexTokenStream popts buffer location of
    PFailed _ -> Left CellLexFailure
    POk _ tokens ->
      let locatedTokens = mapMaybe realTokenSpan tokens
          boundaryLines = reverse (snd (foldl boundary (0 :: Int, []) locatedTokens))
          starts = nub (sort boundaryLines)
       in Right (mapMaybe (sourceItem locatedTokens) (zip starts (drop 1 starts ++ [maxBound])))
  where
    popts = initParserOpts dflags0
    buffer = stringToStringBuffer source
    location = mkRealSrcLoc (mkFastString "<cell>") 1 1

    realTokenSpan (L (RealSrcSpan realSpan _) token)
      | srcSpanStartLine realSpan /= srcSpanEndLine realSpan
          || srcSpanStartCol realSpan /= srcSpanEndCol realSpan
      , not (ordinaryComment token) =
          Just (realSpan, token)
    realTokenSpan _ = Nothing

    boundary (depth, starts) (tokenSpan, token) =
      let starts' = if null starts || (depth == 0 && srcSpanStartCol tokenSpan == 1)
            then srcSpanStartLine tokenSpan : starts
            else starts
          depth' = case token of
            IToparen -> depth + 1
            ITcparen -> max 0 (depth - 1)
            _ -> depth
       in (depth', starts')

    ordinaryComment (ITlineComment _ _) = True
    ordinaryComment (ITblockComment raw _) =
      not ("{-#" `isPrefixOf` raw)
    ordinaryComment _ = False

    sourceItem locatedTokens (startLine, nextLine) = do
      let itemTokens =
            [ pair
            | pair@(tokenSpan, _) <- locatedTokens
            , srcSpanStartLine tokenSpan >= startLine
            , srcSpanStartLine tokenSpan < nextLine
            ]
      firstSpan <- fmap fst (safeHead itemTokens)
      lastSpan <- fmap fst (safeLast itemTokens)
      let startOffset = lineOffset source startLine
          endOffset =
            if nextLine == maxBound
              then length source
              else lineOffset source nextLine
      pure CellSourceItem
        { cellSourceSpan = CellSourceSpan
            { cellStartLine = srcSpanStartLine firstSpan
            , cellStartColumn = srcSpanStartCol firstSpan
            , cellEndLine = srcSpanEndLine lastSpan
            , cellEndColumn = srcSpanEndCol lastSpan
            }
        , cellSourceText = take (endOffset - startOffset) (drop startOffset source)
        , cellSourceDanglingOperator = trailingOperatorText (reverse itemTokens)
        }

    safeHead [] = Nothing
    safeHead (value : _) = Just value

    safeLast [] = Nothing
    safeLast values = Just (last values)

-- Offset of a one-based source line. Callers only supply lines from GHC spans.
lineOffset :: String -> Int -> Int
lineOffset source targetLine = go 1 0 source
  where
    go line offset _ | line == targetLine = offset
    go _ offset [] = offset
    go line offset ('\n' : rest) = go (line + 1) (offset + 1) rest
    go line offset (_ : rest) = go line (offset + 1) rest

-- | Whether an item's REVERSED token list ends with an infix operator that
-- has nothing after it: a plain symbolic operator (@.@, @$@, @<>@, ...) or a
-- backquoted identifier (@`elem`@). @reversedItemTokens@ is the item's own
-- tokens (not the whole cell's) in reverse source order, so the head is the
-- item's last token and the second element, when present, is its
-- second-to-last.
--
-- Deliberately narrow: a trailing operator followed by more of the same item
-- on a later line is not the item's LAST token, so it never reaches here —
-- 'splitCellWithFlags' only calls this on the tokens already isolated to one
-- item.
trailingOperatorText :: [(RealSrcSpan, Token)] -> Maybe String
trailingOperatorText reversedItemTokens = case reversedItemTokens of
  (_, ITbackquote) : (_, nameToken) : _
    | Just name <- backquotedOperandText nameToken -> Just ("`" ++ name ++ "`")
  (_, token) : _ -> symbolicOperatorText token
  [] -> Nothing
  where
    backquotedOperandText = \case
      ITvarid fastString -> Just (unpackFS fastString)
      ITconid fastString -> Just (unpackFS fastString)
      ITqvarid (qualifier, fastString) -> Just (unpackFS qualifier ++ "." ++ unpackFS fastString)
      ITqconid (qualifier, fastString) -> Just (unpackFS qualifier ++ "." ++ unpackFS fastString)
      _ -> Nothing

    symbolicOperatorText = \case
      ITvarsym fastString -> Just (unpackFS fastString)
      ITconsym fastString -> Just (unpackFS fastString)
      ITqvarsym (qualifier, fastString) -> Just (unpackFS qualifier ++ "." ++ unpackFS fastString)
      ITqconsym (qualifier, fastString) -> Just (unpackFS qualifier ++ "." ++ unpackFS fastString)
      -- '.' lexes as its own reserved token (disambiguated from module
      -- qualification by the lexer, which folds a qualifier straight into
      -- 'ITqvarid'/'ITqvarsym' with no separate '.'), but it is still the
      -- ordinary composition operator wherever it stands alone.
      ITdot -> Just "."
      _ -> Nothing

-- | Which wrapper template a verdict selects — a refinement of 'TurnKind': a
-- 'KBind' verdict maps to one of two distinct template shapes depending on
-- whether it actually binds a name (a discarding bind, @_ <- e@, runs for
-- effect and discards, so it needs its own wrapper). Mirrors Rust's own
-- 'TemplateSelector' (@tidepool/runtime/src/session/turn.rs@).
data TemplateSelector = SDecl | SBind | SBindDiscard | SExpr
  deriving (Eq, Show)

-- | Compute the selector a verdict maps to — total over every 'TurnKind',
-- mirroring Rust's @TemplateSelector::for_verdict@.
templateSelectorForVerdict :: TurnKind -> [String] -> TemplateSelector
templateSelectorForVerdict KDecl _       = SDecl
templateSelectorForVerdict KBind []      = SBindDiscard
templateSelectorForVerdict KBind (_ : _) = SBind
templateSelectorForVerdict KExpr _       = SExpr

-- | The wire-name string the extract's @--turn-template <kind>=<file>@ keys
-- its template lookup on. Mirrors Rust's @TemplateSelector::wire_name@ byte
-- for byte.
templateSelectorWireName :: TemplateSelector -> String
templateSelectorWireName SDecl        = "decl"
templateSelectorWireName SBind        = "bind"
templateSelectorWireName SBindDiscard = "binddiscard"
templateSelectorWireName SExpr        = "expr"

-- | Classify @src@ against an already-obtained 'DynFlags' with GHC's own
-- parser (parse-only, no typecheck), letting GHC be the single authority for
-- the decl/bind/expr split (the Rust runtime never parses Haskell itself).
-- Session-independent: takes no session action itself, so both
-- Turn and block-classify modes share it — they cannot fork the verdict a
-- source gets because there is exactly one place the parse happens.
--
-- DECLARATION CONTEXT FIRST, then statement context. A top-level declaration
-- (@f x = e@, a signature @f :: T@, a bare @x = 5@) parses as a decl but fails
-- as a statement. Trying the declaration parse first also disambiguates
-- the genuinely two-faced @f :: T@: in decl
-- context GHC reads it as a signature (@"decl"@), not an annotated expression.
-- Order of precedence:
--
--   * parses as a top-level declaration → @"decl"@ + the declared name(s).
--   * else parses as a statement: @BindStmt@/@LetStmt@ → @"bind"@ + bound
--     names ('collectLStmtBinders'); @BodyStmt@ (a bare expression) → @"expr"@.
--   * else (both fail) → @"expr"@ (the runtime recompiles through the
--     bare-expression path, where GHC re-parses and reports the real error).
classifyWithFlags :: DynFlags -> String -> StmtBinders
classifyWithFlags dflags0 src =
  classifyWithFlagsExact (foldl' xopt_set dflags0 stmtExtensions) src

classifyWithFlagsExact :: DynFlags -> String -> StmtBinders
classifyWithFlagsExact dflags src = fst (classifyWithFlagsExactForm dflags src)

classifyWithFlagsExactForm :: DynFlags -> String -> (StmtBinders, Maybe CellBindingForm)
classifyWithFlagsExactForm dflags src = classifyWithFlagsExactFormUsing dflags src ParseOwnStatement

-- ParseResult is unlifted in GHC. Retain the successful parser-owned facts in
-- a lifted choice and reconstruct its result only when classifying the AST.
data StatementClassificationSource
  = ParseOwnStatement
  | ParsedStatement PState (LStmt GhcPs (LHsExpr GhcPs))

classifyWithFlagsExactFormUsing :: DynFlags -> String
  -> StatementClassificationSource
  -> (StmtBinders, Maybe CellBindingForm)
classifyWithFlagsExactFormUsing dflags src retainedStatement = (verdict, bindingForm)
  where
    verdict = classifyTurn declRes stmtRes modRes
    bindingForm | sbKind verdict == KBind = case stmtRes of
      POk _ statement -> case unLoc statement of
        BindStmt{} -> Just ActionBinding
        LetStmt{} -> Just LetBinding
        RecStmt{} -> Just RecursiveBinding
        _ -> Nothing
      _ -> Nothing
                | otherwise = Nothing
    popts  = initParserOpts dflags
    loc    = mkRealSrcLoc (mkFastString "<turn>") 1 1
    buf    = stringToStringBuffer src
    -- Fresh parser state per attempt (the StringBuffer is immutable, so it
    -- is safe to reuse; the mutable lexer state is not).
    declRes = unP parseDeclaration (initParserState popts buf loc)
    stmtRes = case retainedStatement of
      ParsedStatement parsedState statement -> POk parsedState statement
      ParseOwnStatement -> unP parseStatement (initParserState popts buf loc)
    modRes  = unP GHC.Parser.parseModule (initParserState popts buf loc)

-- A local fixity affects later statements in a do segment but is not part of
-- the type-only Val interface. Refuse it before whole-cell admission rather
-- than reparse a later prepared item with a different associativity.
statementHasLocalFixity :: DynFlags -> String -> Bool
statementHasLocalFixity flags source = case lexTokenStream (initParserOpts flags)
    (stringToStringBuffer source) (mkRealSrcLoc (mkFastString "<turn>") 1 1) of
  POk _ tokens -> any (fixityToken . unLoc) tokens
  PFailed _ -> False
  where
    fixityToken ITinfix = True
    fixityToken ITinfixl = True
    fixityToken ITinfixr = True
    fixityToken _ = False

-- | Classify a whole BLOCK of turns with one parser-defaults session, then run
-- 'classifyWithFlags' once per item against those 'DynFlags'. This IS a whole
-- timed unit — the @--classify@ CLI mode — so unlike
-- the substep it DOES take the timing flag and emit its own phases:
-- @startup@ around 'getLibdir', @ghc_session@ around 'getSessionDynFlags',
-- and @classify@ around the N forced parses (one phase for the whole batch,
-- not one per item). No @typecheck@ phase — that name was always a misnomer
-- for a step that runs no typecheck, and it leaves with the unit that
-- originated it rather than being carried forward here.
classifyBlock :: Bool -> [String] -> IO [StmtBinders]
classifyBlock timing srcs = do
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    (dflags, sessionMs) <- timeSection getSessionDynFlags
    liftIO (emitPhase timing "ghc_session" sessionMs)
    (sbs, classifyMs) <- timeSection (liftIO (mapM (evaluate . classifyWithFlags dflags) srcs))
    liftIO (emitPhase timing "classify" classifyMs)
    pure sbs

-- | Combine the declaration- and statement-context parses into one verdict.
-- Neither context alone is sufficient: a bare @sq 7@ parses (spuriously) as a
-- top-level declaration — an implicit expression-splice, no binder — and a bare
-- signature @sq :: T@ parses as BOTH a signature-declaration and an annotated
-- expression. The precedence below is grounded in the syntax that is actually
-- unambiguous:
--
--   1. @<-@ / top-level @let@ are bind-only markers → @"bind"@.
--   2. A signature (@SigD@) is a declaration (this is how the two-faced
--      @sq :: T@ is resolved — decl, not annotated expr).
--   3. A value/function binding that actually BINDS A NAME (@ValD@ with ≥1
--      harvested binder) → @"decl"@ (@sq x = e@, @x = 5@, @(a,b) = p@).
--   4. A VALID BARE EXPRESSION (@BodyStmt@) → @"expr"@. This runs BEFORE the
--      other-decl catch-all so that @sq 7@ / @filter p xs@ / @pure e@ — which
--      @parseDeclaration@ spuriously accepts as a binder-less splice — classify
--      as expressions, not declarations.
--   5. Any remaining parsed declaration (a non-expression decl the lexical
--      classifier upstream didn't catch, e.g. @deriving instance …@) → @"decl"@.
--   6. Both parses failed → @"expr"@; the runtime recompiles through the
--      bare-expression path and GHC reports the real error loudly.
classifyTurn
  :: ParseResult (LHsDecl GhcPs)
  -> ParseResult (LStmt GhcPs (LHsExpr GhcPs))
  -> ParseResult (Located (HsModule GhcPs))
  -> StmtBinders
classifyTurn declRes stmtRes modRes
  | POk _ lstmt <- stmtRes, isBindStmt lstmt =
      StmtBinders KBind (map occStr (collectLStmtBinders CollNoDictBinders lstmt)) []
  | POk _ ldecl <- declRes, Just sb <- declNameVerdict ldecl = sb
  -- MULTI-DECLARATION items (a sig + its equation, mutually-referencing
  -- equations — one turn, several top-level decls). `parseDeclaration` and
  -- `parseStatement` are SINGLE-item parsers, so before this rule such items
  -- fell all the way to the "expr" fallback, the runtime wrapped them in a
  -- do-block, and the second line's `=` was a parse error — a regression the
  -- old decl-first try-cascade masked and the verdict fast-path exposed. A
  -- headerless decl sequence is a valid module, so the module parse is the
  -- authority for this shape. Gated on >= 2 decls (single-item verdicts keep
  -- their existing rules above/below, byte for byte) and on EVERY decl
  -- declaring a name (a trailing bare call parses as a zero-binder splice
  -- and must keep poisoning nothing — the item is then not a decl batch).
  | POk _ lmod <- modRes
  , decls <- hsmodDecls (unLoc lmod)
  , length decls >= 2
  , verdicts <- map declNameVerdict decls
  , all isJust verdicts =
      StmtBinders
        KDecl
        (nub (concatMap sbBinders (catMaybes verdicts)))
        (concatMap sbDeclItems (catMaybes verdicts))
  | POk _ _ <- stmtRes = StmtBinders KExpr [] []
  | POk _ ldecl <- declRes = StmtBinders KDecl [] (declItems ldecl)
  | otherwise = StmtBinders KExpr [] []

-- | Whether a parsed statement introduces binders (@BindStmt@/@LetStmt@) rather
-- than being a bare expression (@BodyStmt@).
isBindStmt :: LStmt GhcPs (LHsExpr GhcPs) -> Bool
isBindStmt lstmt = case unLoc lstmt of
  BodyStmt{} -> False
  _          -> True

-- | A NAME-DECLARING declaration's verdict, or 'Nothing' if this parsed
-- \"declaration\" declares no name (a zero-binder @ValD@ — a bare application
-- @parseDeclaration@ over-accepts as an implicit splice). A signature (@SigD@)
-- always qualifies, as does a TYPE/CLASS declaration (@TyClD@ — @data@/
-- @newtype@/@type@/@class@): it declares a TYPE name, contributing no VALUE
-- binders (the define path re-derives real exports GHC-side), but it is
-- unambiguously a declaration — without this arm, a model turn defining a
-- block of @data@ types classified as an EXPRESSION and died on a parse
-- error, with the retry prompt then teaching the model that declarations
-- are forbidden (companion dogfood, 2026-08-13). Everything else defers to
-- the caller's expr/other-decl precedence.
declNameVerdict :: LHsDecl GhcPs -> Maybe StmtBinders
declNameVerdict ldecl = case unLoc ldecl of
  SigD _ sig  -> Just (StmtBinders KDecl (sigBinders sig) [])
  TyClD _ _   -> Just (StmtBinders KDecl [] (declItems ldecl))
  ValD _ bind -> case map occStr (collectHsBindBinders CollNoDictBinders bind) of
    []    -> Nothing
    names -> Just (StmtBinders KDecl names (declItems ldecl))
  _           -> Nothing

-- | The names a signature declares (@f, g :: T@ → @["f","g"]@). Only the
-- name-bearing signature forms matter for a session turn.
sigBinders :: Sig GhcPs -> [String]
sigBinders (TypeSig _ names _)      = map (occStr . unLoc) names
sigBinders (ClassOpSig _ _ names _) = map (occStr . unLoc) names
sigBinders _                        = []

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
  , QuasiQuotes, MultilineStrings
  ]

-- | The @--classify@ CLI contract: one verdict per positional file, in argv
-- order. Declaration verdicts also carry the structured export items harvested
-- from the same GHC parse. A signature has binders but no export item; an
-- equation has an 'EValue'. That distinction lets the resident block runner
-- keep signatures with their equations while splitting true redefinitions.
--
-- > {"verdicts":[{"kind":"bind","binders":["x"],"items":[]},
-- >              {"kind":"decl","binders":["sq"],"items":[["EValue","sq"]]},
-- >              {"kind":"expr","binders":[],"items":[]}]}
renderVerdictsJson :: [StmtBinders] -> String
renderVerdictsJson sbs =
  "{\"verdicts\":[" ++ intercalate "," (map renderVerdict sbs) ++ "]}"
  where
    renderVerdict (StmtBinders kind binders items) =
      "{\"kind\":" ++ jsonString (turnKindWireName kind)
        ++ ",\"binders\":[" ++ intercalate "," (map jsonString binders) ++ "]"
        ++ ",\"items\":[" ++ intercalate "," (map renderExportItem items) ++ "]}"

    renderExportItem (EValue name) =
      "[\"EValue\"," ++ jsonString name ++ "]"
    renderExportItem (EType name cons) =
      "[\"EType\"," ++ jsonString name ++ ",["
        ++ intercalate "," (map jsonString cons) ++ "]]"
    renderExportItem (EClass name methods) =
      "[\"EClass\"," ++ jsonString name ++ ",["
        ++ intercalate "," (map jsonString methods) ++ "]]"

--------------------------------------------------------------------------------
-- Turn-mode rich result (--turn) — a tagged variant over the verdict
--------------------------------------------------------------------------------

-- | One bound-value record from a BIND turn: the mint'd 'stableVarId', the
-- thin session iface module it was written under, its closure/data tier, and
-- its rendered type. Carried by the 'Bind' turn result.
data ValueTier
  = ForceData
  | RetainOpaque
  deriving (Eq, Show)

data BoundBinder = BoundBinder
  { bbName        :: String
  , bbVarId       :: Word64
  , bbModule      :: String
  , bbTier        :: ValueTier
  , bbTypeDisplay :: String
  , bbRootHead    :: Maybe NominalHead
  , bbHostAuthority :: Maybe HostBindingAuthority
  } deriving (Eq, Show)

-- | The rich result of a @--turn@ run. 'TDecl' never compiles — its
-- 'toDeclItems' come from a whole-module parse ('extractBindersNamed' over
-- the @--turn@ decl turn's own spliced scratch module), not this module's
-- statement parse, because a decl-batch caller (@--turn-verdict decl@ over N
-- declarations joined into one module) has no single statement to parse.
-- 'TBind'/'TExpr'
-- carry what the selected template variant actually compiled to. The
-- wire-visible tag (the CBOR encoder) is @"Decl"@\/@"Bind"@\/@"Expr"@
-- regardless of these constructor names.
--
-- 'TDecl's @toBinders@ is UNRELIABLE as a verbatim echo of the supplied
-- verdict: a decl-batch verdict (@--turn-verdict decl@, no @:name,name…@
-- suffix) carries an empty binder list, so 'runTurnMode' DERIVES
-- @toBinders@ from 'toDeclItems'' head names ('exportItemName') whenever the
-- verdict itself supplies none. @toBinders@ is therefore never a bare echo
-- of the verdict on the decl path — a 'TurnOut' consumer should read it as
-- "the binders this turn introduces", not as "what the verdict said".
data TurnOut
  = TDecl
      { toBinders   :: [Text]
      , toDeclItems :: [ExportItem]
      , toDeclarationSource :: DeclarationSource
      }
  | TBind
      { toBinders       :: [Text]
      , toVariant       :: Int
      , toBoundBinders  :: [BoundBinder]
      , toAsks          :: [YieldSite]
      , toWrappedSource :: Text
      }
  | TExpr
      { toVariant       :: Int
      , toAsks          :: [YieldSite]
      , toWrappedSource :: Text
      }
  deriving (Eq, Show)

-- | One typed suspension site as JSON. The historical @asks.json@ filename
-- now carries the general answer-plus-live-input contract; ordinary ask/fork
-- sites simply have an empty @inputs@ list.
renderAskJson :: YieldSite -> String
renderAskJson (YieldSite site origin ordinal answer inputs witnesses declaration signatures) =
  "{\"site\":" ++ show site
    ++ ",\"origin\":" ++ jsonString (T.unpack origin)
    ++ ",\"ordinal\":" ++ show ordinal
    ++ ",\"type\":" ++ jsonString (T.unpack (stType answer))
    ++ ",\"modules\":" ++ renderModules (stModules answer)
    ++ ",\"heads\":" ++ renderHeads (stHeads answer)
    ++ ",\"inputs\":[" ++ intercalate "," (map renderSiteType inputs) ++ "]"
    ++ ",\"input_type_witnesses\":[" ++ intercalate "," (map (maybe "null" (maybe "null" jsonString . renderCheckedTypeWitness)) witnesses) ++ "]"
    ++ ",\"reply_declaration\":" ++ maybe "null" (jsonString . T.unpack) declaration
    ++ ",\"request_type_signatures\":" ++ maybe "null" (jsonString . renderRequestTypeSignatures) signatures ++ "}"
  where
    renderSiteType (SiteType ty modules heads) =
      "{\"type\":" ++ jsonString (T.unpack ty)
        ++ ",\"modules\":" ++ renderModules modules
        ++ ",\"heads\":" ++ renderHeads heads ++ "}"
    renderModules modules =
      "[" ++ intercalate "," (map (jsonString . T.unpack) modules) ++ "]"
    renderHeads heads =
      "[" ++ intercalate "," (map renderHead heads) ++ "]"
    renderHead (NominalHead unit modul name) =
      "{\"unit\":" ++ jsonString (T.unpack unit)
        ++ ",\"module\":" ++ jsonString (T.unpack modul)
        ++ ",\"name\":" ++ jsonString (T.unpack name) ++ "}"
