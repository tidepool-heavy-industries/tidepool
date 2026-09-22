{-# LANGUAGE LambdaCase #-}

-- | Binder-name extraction for the turn/classify lanes.
--
-- Given Haskell source, parse it with GHC's own parser (NO typecheck) and
-- report the binders declarations introduce, or classify a single statement
-- as bind\/expr\/decl. The Rust runtime never parses Haskell itself; these
-- GHC-sourced names and verdicts are the only source.
module Tidepool.Binders
  ( ExportItem(..)
  , extractBindersNamed
  , exportItemName
  , declItems
    -- * Statement binders (session-eval bind-vs-expr classification)
  , TurnKind(..)
  , turnKindWireName
  , parseTurnKind
  , StmtBinders(..)
  , extractStmtBinders
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
  , LocatedImport(..)
  , SourcePrologue(..)
  , DeclarationSource(..)
  , CellSourcePlan(..)
  , CellDisplayTarget(..)
  , CellGenericDeclaration(..)
  , installCellDisplayDeclarations
  , omitCellGenericDeclarations
  , omitCellDisplayDeclarations
  , declarationSourceWithTemplate
  , renderDeclarationForTemplate
  , splitCellWithFlags
  , CellAnalysisItem(..)
  , CellAnalysisSourceItem(..)
  , analyzeCellWithFlags
  , analyzeCell
  , renderCellCheckSource
  , CellExpressionPlan(..), ExpressionLiftPlan(..), ExpressionPresentation(..)
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
  ( ParseResult(..)
  , Token(..)
  , initParserState
  , lexTokenStream
  , unP
  )
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Data.FastString (mkFastString)
import GHC.Types.SrcLoc (mkRealSrcLoc)
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
import Tidepool.EffectSchema (NominalHead(..), SiteType(..), YieldSite(..))
import Tidepool.HostBindingAuthority (HostBindingAuthority(..))
import Tidepool.Json (jsonString)
import Tidepool.Timing (timeSection, emitPhase)
import Tidepool.TurnSource (spliceTemplate)

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
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags dflags { importPaths = importPaths dflags ++ includes }
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

-- | One source item in a notebook cell. The text is sliced from the original
-- payload, so quotation bodies and line endings are not reconstructed.
data CellSourceItem = CellSourceItem
  { cellSourceSpan :: CellSourceSpan
  , cellSourceText :: String
  } deriving (Eq, Show)

data CellSplitError
  = CellLexFailure
  | CellPrologueFailure CellSourceSpan String
  | CellHeaderFailure String
  deriving (Eq, Show)

renderCellSplitError :: CellSplitError -> String
renderCellSplitError CellLexFailure = "<cell>:1:1: GHC could not lex the notebook cell"
renderCellSplitError (CellPrologueFailure sourceSpan message) =
  "<cell>:" ++ show (cellStartLine sourceSpan) ++ ":" ++ show (cellStartColumn sourceSpan)
    ++ ": " ++ message
renderCellSplitError (CellHeaderFailure message) =
  "cell check template header: " ++ message

data PragmaKind = LanguagePragma | OptionsGhcPragma
  deriving (Eq, Show)

data LocatedPragma = LocatedPragma
  { locatedPragmaKind :: PragmaKind
  , locatedPragmaSpan :: CellSourceSpan
  , locatedPragmaSource :: String
  } deriving (Eq, Show)

data LocatedImport = LocatedImport
  { locatedImportSpan :: CellSourceSpan
  , locatedImportSource :: String
  } deriving (Eq, Show)

data SourcePrologue = SourcePrologue
  { prologuePragmas :: [LocatedPragma]
  , prologueImports :: [LocatedImport]
  } deriving (Eq, Show)

data DeclarationSource = DeclarationSource
  { declarationPrologue :: SourcePrologue
  , declarationBody :: String
  } deriving (Eq, Show)

data CellSourcePlan = CellSourcePlan
  { cellPlanPrologue :: SourcePrologue
  , cellPlanItems :: [CellAnalysisItem]
  , cellPlanDisplayTargets :: [CellDisplayTarget]
  , cellPlanDisplayAlias :: String
  , cellPlanDeclarationBase :: String
  , cellPlanGenericDeclarations :: [CellGenericDeclaration]
  , cellPlanDisplayDeclarations :: String
  } deriving (Eq, Show)

data CellGenericDeclaration = CellGenericDeclaration
  { genericDeclarationTarget :: String
  , genericDeclarationSource :: String
  } deriving (Eq, Show)

data CellDisplayTarget = CellDisplayTarget
  { displayTargetName :: String
  , displayTargetApplication :: String
  } deriving (Eq, Show)

-- Generated instances do not introduce authored source items or receipt ordinals.
installCellDisplayDeclarations :: String -> CellSourcePlan -> CellSourcePlan
installCellDisplayDeclarations generated plan = plan
  { cellPlanItems = map replaceDeclaration (cellPlanItems plan)
  , cellPlanDisplayDeclarations = generated }
  where
    replaceDeclaration item
      | sbKind (cellAnalysisVerdict item) == KDecl = item
          { cellAnalysisSource = cellPlanDeclarationBase plan
              ++ concatMap genericDeclarationSource (cellPlanGenericDeclarations plan) ++ generated }
      | otherwise = item

omitCellGenericDeclarations :: [String] -> CellSourcePlan -> CellSourcePlan
omitCellGenericDeclarations targets plan =
  installCellDisplayDeclarations (cellPlanDisplayDeclarations plan) plan
    { cellPlanGenericDeclarations = filter ((`notElem` targets) . genericDeclarationTarget)
        (cellPlanGenericDeclarations plan) }

omitCellDisplayDeclarations :: [String] -> CellSourcePlan -> CellSourcePlan
omitCellDisplayDeclarations targets plan =
  let retained = plan { cellPlanDisplayTargets = filter ((`notElem` targets) . displayTargetName)
                         (cellPlanDisplayTargets plan) }
   in installCellDisplayDeclarations
        (concatMap (opaqueDisplayInstance (cellPlanDisplayAlias retained)) (cellPlanDisplayTargets retained)) retained

opaqueDisplayInstance :: String -> CellDisplayTarget -> String
opaqueDisplayInstance qualifier target = "\ninstance {-# OVERLAPPABLE #-} " ++ qualifier ++ ".Display "
  ++ displayTargetApplication target ++ " where\n  displayTree _ = "
  ++ qualifier ++ ".TextLeaf (" ++ qualifier ++ "Text.pack \"<opaque>\")\n"

emptyPrologue :: SourcePrologue
emptyPrologue = SourcePrologue [] []

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
data CellAnalysisItem = CellAnalysisItem
  { cellAnalysisSpan :: CellSourceSpan
  , cellAnalysisSource :: String
  , cellAnalysisVerdict :: StmtBinders
  , cellAnalysisSourceItems :: [CellAnalysisSourceItem]
  } deriving (Eq, Show)

data ExpressionLiftPlan = ExpressionEffectful | ExpressionPure
  deriving (Eq, Show)

data ExpressionPresentation = ExpressionRendered | ExpressionOpaque
  deriving (Eq, Show)

-- | Compiler-owned execution decision for one expression item. The key is
-- the reserved local binder whose zonked type supplied this evidence.
data CellExpressionPlan = CellExpressionPlan
  { expressionPlanKey :: String
  , expressionPlanLift :: ExpressionLiftPlan
  , expressionPlanPresentation :: ExpressionPresentation
  , expressionPlanType :: String
  , expressionPlanHeads :: [NominalHead]
  } deriving (Eq, Show)

-- | One original source item retained beneath its execution item. Declaration
-- items are compiled as one group, but receipts still need their individual
-- ordinals and spans.
data CellAnalysisSourceItem = CellAnalysisSourceItem
  { cellAnalysisSourceOrdinal :: Int
  , cellAnalysisSourceSpan :: CellSourceSpan
  , cellAnalysisSourceKind :: TurnKind
  } deriving (Eq, Show)

-- | A post-zonk type captured for a statement binder during the whole-cell
-- check. The rendered type is replanted into the later staged compile while
-- nominal heads let the consumer distinguish same-cell declarations from
-- already-installed names.
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
analyzeCellWithFlags dflags template source = do
  flags <- cellEffectiveFlags dflags template source
  pure $ do
    effective <- flags
    lexical <- splitCellWithFlags effective source
    (prologue, headerItems, bodyItems) <- collectPrologue effective lexical
    let firstBodyLine = case bodyItems of
          item : _ -> cellStartLine (cellSourceSpan item)
          [] -> maxBound
        bodySource = blankBeforeLine firstBodyLine source
    body <- splitCellWithFlags effective bodySource
    let classified = zipWith (classify effective) [length headerItems..] body
        genericAlias = freshAlias "TidepoolCompilerGeneric" source
        displayAlias = freshAlias "TidepoolCompilerDisplay" source
        generated = automaticGenericDeclarations effective genericAlias classified
        grouped = groupDeclarations headerItems classified ""
        targets = automaticDisplayTargets effective classified
        generatedImports =
          [ LocatedImport (CellSourceSpan 1 1 1 1) ("import qualified GHC.Generics as " ++ genericAlias)
          | not (null generated) ] ++
          [ LocatedImport (CellSourceSpan 1 1 1 1) ("import qualified Tidepool.Inspection as " ++ displayAlias)
          | not (null targets) ] ++
          [ LocatedImport (CellSourceSpan 1 1 1 1) ("import qualified Data.Text as " ++ displayAlias ++ "Text")
          | not (null targets) ]
        plan = CellSourcePlan
          { cellPlanPrologue = prologue { prologueImports = prologueImports prologue ++ generatedImports }
          , cellPlanItems = grouped
          , cellPlanDisplayTargets = targets
          , cellPlanDisplayAlias = displayAlias
          , cellPlanDeclarationBase = concat
              [ cellAnalysisSource item | item <- grouped, sbKind (cellAnalysisVerdict item) == KDecl ]
          , cellPlanGenericDeclarations = generated
          , cellPlanDisplayDeclarations = ""
          }
    pure (installCellDisplayDeclarations (concatMap (opaqueDisplayInstance displayAlias) targets) plan)
  where
    freshAlias candidate authoredSource
      | candidate `isInfixOf` authoredSource = freshAlias (candidate ++ "X") authoredSource
      | otherwise = candidate
    classify effective ordinal item =
      let verdict = classifyWithFlagsExact effective (cellSourceText item)
       in CellAnalysisItem
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
      }
    groupDeclarations headerItems classified generated =
      case partition isDeclaration classified of
        ([], executable) | null headerItems -> executable
        (declarations, executable) -> declarationGroup headerItems declarations generated : executable
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
            }
    locatedDeclaration item =
      "{-# LINE " ++ show (cellStartLine (cellAnalysisSpan item))
      ++ " \"<cell>\" #-}\n"
      ++ cellAnalysisSource item
      ++ if null (cellAnalysisSource item)
          || last (cellAnalysisSource item) == '\n'
        then ""
        else "\n"

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

automaticDisplayTargets :: DynFlags -> [CellAnalysisItem] -> [CellDisplayTarget]
automaticDisplayTargets flags declarations = case unP GHC.Parser.parseModule parserState of
  PFailed _ -> []
  POk _ parsed ->
    let decls = hsmodDecls (unLoc parsed)
     in [ CellDisplayTarget (targetName target) (targetApplication target)
        | target <- mapMaybe genericTarget decls ]
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
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    liftIO (analyzeCellWithFlags dflags template source)

-- | Extract the same located header for a declaration turn without
-- reclassifying the already-checked declaration body.
declarationSourceWithTemplate
  :: String -> String -> IO (Either CellSplitError DeclarationSource)
declarationSourceWithTemplate template source = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    liftIO $ do
      flags <- cellEffectiveFlags dflags template source
      pure $ do
        effective <- flags
        lexical <- splitCellWithFlags effective source
        (prologue, _, bodyItems) <- collectPrologue effective lexical
        let firstBodyLine = case bodyItems of
              item : _ -> cellStartLine (cellSourceSpan item)
              [] -> maxBound
        pure (DeclarationSource prologue (blankBeforeLine firstBodyLine source))

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
       in Right (spliceTemplate withPragmas
            (imports ++ declarationBody source) "")
  where
    moduleHeader line = "module " `isPrefixOf` dropWhile isSpace line

-- | Fill the runtime-authored whole-cell checking template. The template owns
-- imports, the exact effect row, and expression admissibility; this function
-- only places GHC-classified source and compiler-reserved pin aliases.
--
-- The two literal placeholders are intentionally the entire template
-- vocabulary. Missing or duplicate placeholders are rejected by the worker
-- entry point before compilation.
renderCellCheckSource :: String -> CellSourcePlan -> Either String String
renderCellCheckSource template plan = do
  withPragmas <- replaceOnce "{{CELL_PRAGMAS}}" pragmas template
  withImports <- replaceOnce "{{CELL_IMPORTS}}" imports withPragmas
  withDecls <- replaceOnce "{{CELL_DECLS}}" declarations withImports
  renderedBody <- body
  replaceOnce "{{CELL_BODY}}" renderedBody withDecls
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

    replaceOnce needle replacement haystack =
      case breakOn needle haystack of
        Nothing -> Left ("cell check template is missing " ++ needle)
        Just (before, after)
          | needle `isInfixOf` after ->
              Left ("cell check template contains " ++ needle ++ " more than once")
          | otherwise -> Right (before ++ replacement ++ after)

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
          tokenSpans = map fst locatedTokens
          boundaryLines = reverse (snd (foldl boundary (0 :: Int, []) locatedTokens))
          starts = nub (sort boundaryLines)
       in Right (mapMaybe (sourceItem tokenSpans) (zip starts (drop 1 starts ++ [maxBound])))
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

    sourceItem tokenSpans (startLine, nextLine) = do
      firstSpan <- firstAtOrAfter startLine tokenSpans
      lastSpan <- lastBefore nextLine tokenSpans
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
        }

    firstAtOrAfter line =
      safeHead . filter ((>= line) . srcSpanStartLine)

    lastBefore line =
      safeLast . filter ((< line) . srcSpanStartLine)

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
-- 'extractStmtBinders' (one src, one session) and 'classifyBlock' (N srcs,
-- one session) can share it — they cannot fork the verdict a src gets
-- because there is exactly one place the parse happens.
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
classifyWithFlagsExact dflags src = classifyTurn declRes stmtRes modRes
  where
    popts  = initParserOpts dflags
    loc    = mkRealSrcLoc (mkFastString "<turn>") 1 1
    buf    = stringToStringBuffer src
    -- Fresh parser state per attempt (the StringBuffer is immutable, so it
    -- is safe to reuse; the mutable lexer state is not).
    declRes = unP parseDeclaration (initParserState popts buf loc)
    stmtRes = unP parseStatement   (initParserState popts buf loc)
    modRes  = unP GHC.Parser.parseModule (initParserState popts buf loc)

-- | Boot a GHC session and classify one turn's source. A SUBSTEP, not a whole
-- timed unit: it emits no phases of its own — a phase's owner has to be whatever knows it
-- is a whole unit, and both surviving callers ('runTurnMode''s classify
-- substep, and the block-classify unit wrapping a batch of these) time it
-- themselves. The 'evaluate' force stays: the caller's phase measures wall
-- clock around this call, and an unforced thunk would let that phase measure
-- nothing.
extractStmtBinders :: String -> IO StmtBinders
extractStmtBinders src = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    liftIO (evaluate (classifyWithFlags dflags src))

-- | Classify a whole BLOCK of turns in one process: boot exactly ONE GHC
-- session (unlike N calls to 'extractStmtBinders', which would boot N), then
-- run 'classifyWithFlags' once per item against that single session's
-- 'DynFlags'. This IS a whole timed unit — the @--classify@ CLI mode — so unlike
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
renderAskJson (YieldSite site origin ordinal answer inputs declaration) =
  "{\"site\":" ++ show site
    ++ ",\"origin\":" ++ jsonString (T.unpack origin)
    ++ ",\"ordinal\":" ++ show ordinal
    ++ ",\"type\":" ++ jsonString (T.unpack (stType answer))
    ++ ",\"modules\":" ++ renderModules (stModules answer)
    ++ ",\"heads\":" ++ renderHeads (stHeads answer)
    ++ ",\"inputs\":[" ++ intercalate "," (map renderSiteType inputs) ++ "]"
    ++ ",\"reply_declaration\":" ++ maybe "null" (jsonString . T.unpack) declaration ++ "}"
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
