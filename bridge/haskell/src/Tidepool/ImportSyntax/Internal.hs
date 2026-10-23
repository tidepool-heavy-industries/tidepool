-- | Syntax correspondence for one captured generated root. These values carry
-- no permission to load an interface, admit source, or execute native code.
module Tidepool.ImportSyntax.Internal
  ( ImportDescriptor, describeImport
  , ImportOccurrenceId(..), ImportOccurrence(..)
  , importOccurrenceId, importOccurrenceDescriptor, importOccurrenceSpan
  , CapturedImportSyntax(..), captureImportSyntax, capturedImportOccurrences, capturedParsedSource
  , DriverImportSyntax(..), matchDriverImports, driverImportOccurrences, driverParsedModule
  , RefinedImportSyntax, beginImportRefinement, refinedParsedModule, refinedImportOccurrences
  , ImportLineage(..), importOccurrenceLineage
  , ImportRefinement(..), mapRefinedImportsM, insertSyntheticPrelude, mapImportPreservingSource
  , ImportOrigin(..), ImportRendering(..)
  , AuthoredImports(..), InheritedImports(..), ProtectedImports(..)
  , authoredImportsRendering, inheritedImportsRendering, protectedImportsRendering
  , CellSourceSpan(..), LocatedImport(..), ImportIntent(..)
  , locatedImportSpan, locatedImportSource, locatedImportIntent, locatedImportNamespaces
  , ImportSyntaxError(..)
  ) where

import Control.Monad (unless, forM)
import GHC (GhcPs, ImportDecl(..), HsModule(..), Located, ParsedModule(..), ParsedSource, unLoc)
import GHC.Data.FastString (FastString)
import GHC.Hs
  ( IE(..), IEWrappedName(..), IEWildcard(..), ImportDeclQualifiedStyle(..)
  , ImportListInterpretation(..), ideclImplicit, LImportDecl, LIE, simpleImportDecl )
import GHC.Parser.Annotation (getLocA, noLocA)
import GHC.Types.Name.Occurrence (OccName)
import GHC.Types.Name.Reader (RdrName(..))
import GHC.Types.PkgQual (RawPkgQual(..))
import GHC.Types.SourceText (sl_fs)
import GHC.Types.SrcLoc (SrcSpan, GenLocated(..), getBufSpan)
import GHC.Unit.Module (ModuleName, moduleNameString, mkModuleName)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Utils.Outputable (defaultSDocContext, ppr, showSDocOneLine)

-- Only actual issuer modules import this representation. Public renderers
-- accept opaque values; they cannot label arbitrary text as prior/protected.
data ImportOrigin = AuthoredCurrent | InheritedLexical | ProtectedCompiler
  deriving (Eq, Ord, Show)

data ImportRendering = ImportRendering String [(Int, Int, ImportOrigin)]
  deriving (Eq, Show)

newtype AuthoredImports = AuthoredImports String
newtype InheritedImports = InheritedImports String
newtype ProtectedImports = ProtectedImports String

authoredImportsRendering :: AuthoredImports -> ImportRendering
authoredImportsRendering (AuthoredImports source) =
  ImportRendering source [(0,length source,AuthoredCurrent)]

inheritedImportsRendering :: InheritedImports -> ImportRendering
inheritedImportsRendering (InheritedImports source) =
  ImportRendering source [(0,length source,InheritedLexical)]

protectedImportsRendering :: ProtectedImports -> ImportRendering
protectedImportsRendering (ProtectedImports source) =
  ImportRendering source [(0,length source,ProtectedCompiler)]

data CellSourceSpan = CellSourceSpan
  { cellStartLine :: Int
  , cellStartColumn :: Int
  , cellEndLine :: Int
  , cellEndColumn :: Int
  } deriving (Eq, Show)

data ImportIntent
  = AuthoredSourceImport ModuleName RawPkgQual
  | InheritedLexicalImport
  | ProtectedCompilerImport

instance Eq ImportIntent where
  AuthoredSourceImport leftOwner leftQualifier == AuthoredSourceImport rightOwner rightQualifier =
    leftOwner == rightOwner && sameQualifier leftQualifier rightQualifier
    where
      sameQualifier NoRawPkgQual NoRawPkgQual = True
      sameQualifier (RawPkgQual left) (RawPkgQual right) = left == right
      sameQualifier _ _ = False
  InheritedLexicalImport == InheritedLexicalImport = True
  ProtectedCompilerImport == ProtectedCompilerImport = True
  _ == _ = False

instance Show ImportIntent where
  show (AuthoredSourceImport owner qualifier) =
    "AuthoredSourceImport " ++ moduleNameString owner ++ " "
      ++ showSDocOneLine defaultSDocContext (ppr qualifier)
  show InheritedLexicalImport = "InheritedLexicalImport"
  show ProtectedCompilerImport = "ProtectedCompilerImport"

-- Positional representation keeps public readonly accessors from also being
-- record-update labels that could replace an issued occurrence's intent.
data LocatedImport = LocatedImport CellSourceSpan String ImportIntent [ModuleName]
  deriving (Eq, Show)

locatedImportSpan :: LocatedImport -> CellSourceSpan
locatedImportSpan (LocatedImport location _ _ _) = location

locatedImportSource :: LocatedImport -> String
locatedImportSource (LocatedImport _ source _ _) = source

locatedImportIntent :: LocatedImport -> ImportIntent
locatedImportIntent (LocatedImport _ _ intent _) = intent

locatedImportNamespaces :: LocatedImport -> [ModuleName]
locatedImportNamespaces (LocatedImport _ _ _ namespaces) = namespaces

-- Keep GHC's occurrence namespace, including record-field parent identity.
-- Prettyprinted text and source annotations are deliberately not keys.
data ImportName
  = UnqualifiedImportName OccName
  | QualifiedImportName ModuleName OccName
  deriving (Eq)

data WrappedImportName
  = OrdinaryImportName ImportName
  | DefaultImportName ImportName
  | PatternImportName ImportName
  | TypeImportName ImportName
  deriving (Eq)

data ImportEntry
  = ImportVariable WrappedImportName
  | ImportAbstractThing WrappedImportName
  | ImportAllThing WrappedImportName
  | ImportThingWith WrappedImportName (Maybe Int) [WrappedImportName]
  deriving (Eq)

data ImportQualification = ImportUnqualified | ImportQualifiedPre | ImportQualifiedPost
  deriving (Eq)

data ImportSelection = ImportExactly [ImportEntry] | ImportHiding [ImportEntry]
  deriving (Eq)

data ImportDescriptor = ImportDescriptor
  ModuleName (Maybe FastString) Bool Bool Bool ImportQualification
  (Maybe ModuleName) (Maybe ImportSelection)
  deriving (Eq)

instance Show ImportDescriptor where
  show _ = "ImportDescriptor"

-- Fail closed if a future GHC syntax constructor needs a correspondence rule.
data ImportSyntaxError
  = UnsupportedImportDeclaration
  | UnsupportedImportEntry
  | UnsupportedImportName
  | InvalidImportWildcard Int
  | DriverImportCountChanged Int Int
  | DriverImportChanged ImportOccurrenceId
  | RefinementRequiresUnqualified ImportOccurrenceId
  | RefinementChangedImports
  | RepeatedSyntheticPrelude
  deriving (Eq, Show)

describeImport :: ImportDecl GhcPs -> Either ImportSyntaxError ImportDescriptor
describeImport declaration@ImportDecl{} = do
  selection <- case ideclImportList declaration of
    Nothing -> Right Nothing
    Just (interpretation, entries) -> do
      described <- mapM (describeEntry . unLoc) (unLoc entries)
      pure (Just (case interpretation of
        Exactly -> ImportExactly described
        EverythingBut -> ImportHiding described))
  pure (ImportDescriptor (unLoc (ideclName declaration))
    (case ideclPkgQual declaration of
      NoRawPkgQual -> Nothing
      RawPkgQual literal -> Just (sl_fs literal))
    (ideclSource declaration == IsBoot) (ideclSafe declaration)
    (ideclImplicit (ideclExt declaration))
    (case ideclQualified declaration of
      NotQualified -> ImportUnqualified
      QualifiedPre -> ImportQualifiedPre
      QualifiedPost -> ImportQualifiedPost)
    (unLoc <$> ideclAs declaration) selection)
describeImport _ = Left UnsupportedImportDeclaration

describeEntry :: IE GhcPs -> Either ImportSyntaxError ImportEntry
describeEntry entry = case entry of
  IEVar _ wrapped _ -> ImportVariable <$> describeWrapped (unLoc wrapped)
  IEThingAbs _ wrapped _ -> ImportAbstractThing <$> describeWrapped (unLoc wrapped)
  IEThingAll _ wrapped _ -> ImportAllThing <$> describeWrapped (unLoc wrapped)
  IEThingWith _ wrapped wildcard children _ -> do
    parent <- describeWrapped (unLoc wrapped)
    members <- mapM (describeWrapped . unLoc) children
    position <- case wildcard of
      NoIEWildcard -> Right Nothing
      IEWildcard index
        | index >= 0 && index <= length children -> Right (Just index)
        | otherwise -> Left (InvalidImportWildcard index)
    pure (ImportThingWith parent position members)
  _ -> Left UnsupportedImportEntry

describeWrapped :: IEWrappedName GhcPs -> Either ImportSyntaxError WrappedImportName
describeWrapped wrapped = case wrapped of
  IEName _ name -> OrdinaryImportName <$> describeName (unLoc name)
  IEDefault _ name -> DefaultImportName <$> describeName (unLoc name)
  IEPattern _ name -> PatternImportName <$> describeName (unLoc name)
  IEType _ name -> TypeImportName <$> describeName (unLoc name)
  _ -> Left UnsupportedImportName

describeName :: RdrName -> Either ImportSyntaxError ImportName
describeName name = case name of
  Unqual occurrence -> Right (UnqualifiedImportName occurrence)
  Qual qualifier occurrence -> Right (QualifiedImportName qualifier occurrence)
  -- Orig/Exact require a certified transformation, never raw parser matching.
  _ -> Left UnsupportedImportName

data ImportOccurrenceId
  = CapturedImportId Int
  | NarrowedImportId ImportOccurrenceId
  | QualifiedImportId ImportOccurrenceId
  | SyntheticPreludeId
  deriving (Eq, Ord, Show)

data ImportLineage
  = CapturedImport
  | NarrowedImport ImportOccurrenceId
  | QualifiedImport ImportOccurrenceId
  | SyntheticPrelude
  deriving (Eq, Show)

data ImportOccurrence = ImportOccurrence ImportOccurrenceId ImportDescriptor SrcSpan ImportLineage
  deriving (Eq)

importOccurrenceId :: ImportOccurrence -> ImportOccurrenceId
importOccurrenceId (ImportOccurrence identity _ _ _) = identity

importOccurrenceDescriptor :: ImportOccurrence -> ImportDescriptor
importOccurrenceDescriptor (ImportOccurrence _ descriptor _ _) = descriptor

importOccurrenceSpan :: ImportOccurrence -> SrcSpan
importOccurrenceSpan (ImportOccurrence _ _ location _) = location

importOccurrenceLineage :: ImportOccurrence -> ImportLineage
importOccurrenceLineage (ImportOccurrence _ _ _ lineage) = lineage

data CapturedImportSyntax = CapturedImportSyntax ParsedSource [ImportOccurrence]

-- This is syntax equality only. The admission owner separately binds the
-- actual source bytes, root, effective policy and compilation attempt.
instance Eq CapturedImportSyntax where
  CapturedImportSyntax _ left == CapturedImportSyntax _ right = left == right

captureImportSyntax :: Located (HsModule GhcPs) -> Either ImportSyntaxError CapturedImportSyntax
captureImportSyntax parsed = CapturedImportSyntax parsed <$> mapM describe
  (zip [0..] (hsmodImports (unLoc parsed)))
  where
    describe (index, located) = do
      descriptor <- describeImport (unLoc located)
      pure (ImportOccurrence (CapturedImportId index) descriptor (getLocA located) CapturedImport)

capturedImportOccurrences :: CapturedImportSyntax -> [ImportOccurrence]
capturedImportOccurrences (CapturedImportSyntax _ occurrences) = occurrences

capturedParsedSource :: CapturedImportSyntax -> ParsedSource
capturedParsedSource (CapturedImportSyntax parsed _) = parsed

data DriverImportSyntax = DriverImportSyntax CapturedImportSyntax ParsedModule [ImportOccurrence]

-- GHC may prepend a LINE pragma to targetContents. Correspondence uses the
-- complete ordered syntax, then lends each distinct raw identity its actual
-- driver location. It never compares offsets from different source buffers.
matchDriverImports :: CapturedImportSyntax -> ParsedModule
  -> Either ImportSyntaxError DriverImportSyntax
matchDriverImports capturedSyntax@(CapturedImportSyntax _ captured) parsed = do
  let actual = hsmodImports (unLoc (pm_parsed_source parsed))
  unless (length captured == length actual)
    (Left (DriverImportCountChanged (length captured) (length actual)))
  DriverImportSyntax capturedSyntax parsed <$> mapM bind (zip captured actual)
  where
    bind (ImportOccurrence identity expected _ lineage, located) = do
      actual <- describeImport (unLoc located)
      unless (actual == expected) (Left (DriverImportChanged identity))
      pure (ImportOccurrence identity actual (getLocA located) lineage)

driverImportOccurrences :: DriverImportSyntax -> [ImportOccurrence]
driverImportOccurrences (DriverImportSyntax _ _ occurrences) = occurrences

driverParsedModule :: DriverImportSyntax -> ParsedModule
driverParsedModule (DriverImportSyntax _ parsed _) = parsed

-- Refinement replaces the parsed source inside the same module observation;
-- it cannot attach a different summary or an unrelated whole parsed module.
data RefinedImportSyntax = RefinedImportSyntax CapturedImportSyntax ParsedModule [ImportOccurrence]

beginImportRefinement :: DriverImportSyntax -> RefinedImportSyntax
beginImportRefinement (DriverImportSyntax captured parsed occurrences) =
  RefinedImportSyntax captured parsed occurrences

refinedParsedModule :: RefinedImportSyntax -> ParsedModule
refinedParsedModule (RefinedImportSyntax _ parsed _) = parsed

refinedImportOccurrences :: RefinedImportSyntax -> [ImportOccurrence]
refinedImportOccurrences (RefinedImportSyntax _ _ occurrences) = occurrences

-- This closed recipe is available only to the actual checked-prefix issuer.
-- A qualified clone always retains the parent's complete selection.
data ImportRefinement = KeepImport | NarrowUnqualified [LIE GhcPs]

mapRefinedImportsM :: (LImportDecl GhcPs -> IO ImportRefinement)
  -> RefinedImportSyntax -> IO RefinedImportSyntax
mapRefinedImportsM choose (RefinedImportSyntax captured parsed occurrences) = do
  let syntax = unLoc (pm_parsed_source parsed)
      imports = hsmodImports syntax
  unless (length imports == length occurrences) (fail (show RefinementChangedImports))
  changed <- forM (zip occurrences imports) $ \(occurrence, located@(L location declaration)) -> do
    recipe <- choose located
    case recipe of
      KeepImport -> pure [(occurrence,located)]
      NarrowUnqualified entries -> do
        unless (ideclQualified declaration == NotQualified)
          (fail (show (RefinementRequiresUnqualified (importOccurrenceId occurrence))))
        let parent = importOccurrenceId occurrence
            narrowed = declaration {ideclImportList = Just (Exactly,noLocA entries)}
            qualified = declaration {ideclQualified = QualifiedPre}
        narrowedDescriptor <- either (fail . show) pure (describeImport narrowed)
        qualifiedDescriptor <- either (fail . show) pure (describeImport qualified)
        pure
          [ (ImportOccurrence (NarrowedImportId parent) narrowedDescriptor (getLocA located)
                (NarrowedImport parent), L location narrowed)
          , (ImportOccurrence (QualifiedImportId parent) qualifiedDescriptor (getLocA located)
                (QualifiedImport parent), L location qualified)
          ]
  let flattened = concat changed
      source = case pm_parsed_source parsed of
        L location _ -> L location syntax {hsmodImports = map snd flattened}
  pure (RefinedImportSyntax captured (parsed {pm_parsed_source = source}) (map fst flattened))

insertSyntheticPrelude :: RefinedImportSyntax -> Either String RefinedImportSyntax
insertSyntheticPrelude (RefinedImportSyntax captured parsed occurrences) = do
  let syntax = unLoc (pm_parsed_source parsed)
      imports = hsmodImports syntax
  unless (all ((/= mkModuleName "Prelude") . unLoc . ideclName . unLoc) imports)
    (Left (show RepeatedSyntheticPrelude))
  let initial = simpleImportDecl (mkModuleName "Prelude")
      declaration = initial {ideclExt = (ideclExt initial) {ideclImplicit = True}}
      located = noLocA declaration
  descriptor <- either (Left . show) Right (describeImport declaration)
  let occurrence = ImportOccurrence SyntheticPreludeId descriptor (getLocA located) SyntheticPrelude
      source = case pm_parsed_source parsed of
        L location _ -> L location syntax {hsmodImports = located : imports}
  pure (RefinedImportSyntax captured (parsed {pm_parsed_source = source}) (occurrence : occurrences))

mapImportPreservingSource :: (ParsedSource -> IO ParsedSource)
  -> RefinedImportSyntax -> IO RefinedImportSyntax
mapImportPreservingSource transform (RefinedImportSyntax captured parsed occurrences) = do
  changed <- transform (pm_parsed_source parsed)
  let before = hsmodImports (unLoc (pm_parsed_source parsed))
      after = hsmodImports (unLoc changed)
      same (left,right) = do
        leftDescriptor <- describeImport (unLoc left)
        rightDescriptor <- describeImport (unLoc right)
        unless (leftDescriptor == rightDescriptor
            && getLocA left == getLocA right
            && getBufSpan (getLocA left) == getBufSpan (getLocA right))
          (Left RefinementChangedImports)
  unless (length before == length after) (fail (show RefinementChangedImports))
  either (fail . show) pure (mapM_ same (zip before after))
  pure (RefinedImportSyntax captured (parsed {pm_parsed_source = changed}) occurrences)
