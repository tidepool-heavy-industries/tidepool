-- | Source assembly for the fused classify-and-compile turn request.
--
-- This remains in the compiler worker because binder names come from GHC's
-- classification result and must be inserted before the same resident worker
-- compiles the selected wrapper. Moving it to the launcher would require a
-- second worker round trip. Rust owns template selection; this module protects
-- fixed compiler default identities and places source before authored syntax
-- enters the selected module.
module Tidepool.TurnSource
  ( extractModuleName
  , spliceTemplate
  , renderImportBinder
  , replaceTemplateMarker
  , generatedScaffoldModuleName
  , renameScaffoldModuleHeader
  , CompilerDefaultRecipe, emptyCompilerDefaultRecipe, captureCompilerDefaultRecipe
  , qualifyCompilerDefault, qualifyCompilerDefaultWithLineOffset
  , preambleDefaultDeclaration, preambleImportMarker, preambleDefaultMarker, importQualifierNamespaces
  , PreparedDeclarationTemplate, prepareDeclarationTemplate, renderPreparedDeclaration
  , ImportOrigin(..)
  , ImportRendering, literalImportRendering, locatedImportRendering, appendImportRendering
  , replaceImportRendering, importRenderingSource, importRenderingOrigins
  , spliceImportRendering
  , qualifyCompilerDefaultRendering, prepareDeclarationTemplateWithImports
  , renderPreparedDeclarationWithImports
  , preparedDeclarationImportOffset
  ) where

import GHC (GhcPs, ImportDecl(..), ModuleName, hsmodImports, hsmodName, unLoc, moduleNameString, mkModuleName)
import GHC.Builtin.Types (intTyConName, doubleTyConName)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Driver.Session (DynFlags, xopt_set)
import GHC.LanguageExtensions (Extension(PackageImports))
import GHC.Parser (parseHeader)
import GHC.Parser.Annotation (getLocA)
import GHC.Data.Strict qualified as Strict
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP, getPsErrorMessages)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Data.FastString (mkFastString)
import GHC.Types.SrcLoc (SrcSpan, RealSrcLoc, mkRealSrcLoc, getBufSpan, bufSpanStart, bufPos)
import GHC.Types.Error (isEmptyMessages)
import GHC.Types.Name (nameModule, nameOccName)
import GHC.Unit.Types (moduleUnit, moduleName, unitString)
import GHC.Types.Name.Occurrence (isSymOcc, mkVarOcc, occNameString)
import Data.Char (isSpace)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix)
import Data.Maybe (maybeToList)
import qualified Data.Set as Set
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.ByteString as BS
import Tidepool.ExtractUtil (shaHex)

-- Import origin is recorded before rendering merges compiler and authored
-- syntax. It classifies source reachability, never TYPE or native permission.
data ImportOrigin = AuthoredCurrent | InheritedLexical | ProtectedCompiler
  deriving (Eq, Ord, Show)

-- Physical decoded-character ranges move with the source edits that render
-- imports. GHC's BufPos uses these offsets, independently of LINE pragmas.
data ImportRendering = ImportRendering String [(Int, Int, ImportOrigin)]
  deriving (Eq, Show)

literalImportRendering :: String -> ImportRendering
literalImportRendering source = ImportRendering source []

locatedImportRendering :: ImportOrigin -> String -> ImportRendering
locatedImportRendering origin source = ImportRendering source [(0, length source, origin)]

importRenderingSource :: ImportRendering -> String
importRenderingSource (ImportRendering source _) = source

importRenderingOrigins :: ImportRendering -> [(Int, Int, ImportOrigin)]
importRenderingOrigins (ImportRendering _ origins) = origins

appendImportRendering :: ImportRendering -> ImportRendering -> ImportRendering
appendImportRendering (ImportRendering left before) (ImportRendering right after) =
  ImportRendering (left ++ right)
    (before ++ [(start + length left, end + length left, origin) | (start,end,origin) <- after])

replaceImportRendering :: String -> ImportRendering -> ImportRendering -> Either String ImportRendering
replaceImportRendering marker (ImportRendering replacement added) (ImportRendering source existing) = do
  let (before, after) = T.breakOn (T.pack marker) (T.pack source)
      suffix = T.drop (length marker) after
      start = length (T.unpack before)
      end = start + length marker
      delta = length replacement - length marker
      move (first,last',origin)
        | last' <= start = Right (first,last',origin)
        | first >= end = Right (first+delta,last'+delta,origin)
        | otherwise = Left "source rendering edit crosses an issued import occurrence"
  if null marker || T.null after || T.pack marker `T.isInfixOf` suffix
    then Left "checked recipe marker is missing or duplicated"
    else do
      moved <- mapM move existing
      pure (ImportRendering (T.unpack before ++ replacement ++ T.unpack suffix)
        (moved ++ [(first+start,last'+start,origin) | (first,last',origin) <- added]))

spliceImportRendering :: ImportRendering -> String -> String -> Either String ImportRendering
spliceImportRendering (ImportRendering template origins) body binders = do
  let boundaries = [length (T.unpack before) | marker <- ["{{TURN_STMT}}","{{TURN}}","{{BINDERS}}"]
        , let (before,after) = T.breakOn (T.pack marker) (T.pack template), not (T.null after)]
  if any (\(_,end,_) -> any (< end) boundaries) origins
    then Left "authored body insertion precedes an issued import occurrence"
    else pure (ImportRendering (spliceTemplate template body binders) origins)

-- The import slot and protected default recipe have independent Rust/Haskell
-- ABI markers. Installer templates need imports without primitive defaulting.
preambleDefaultDeclaration :: String
preambleDefaultDeclaration = preambleImportMarker ++ preambleDefaultMarker ++ "default (Int, Double, Text)\n"

preambleImportMarker :: String
preambleImportMarker = "-- tidepool-preamble-imports-v1\n"

preambleDefaultMarker :: String
preambleDefaultMarker = "-- tidepool-preamble-defaults-v1\n"

-- Only the protected template supplies this recipe. Keep native namespace facts
-- from its parsed header; later rendering adds the parsed authored/import facts.
data CompilerDefaultRecipe
  = NoCompilerDefault
  | PrimitiveCompilerDefault [ModuleName]
  deriving (Eq, Show)

emptyCompilerDefaultRecipe :: CompilerDefaultRecipe
emptyCompilerDefaultRecipe = NoCompilerDefault

importQualifierNamespaces :: ImportDecl GhcPs -> [ModuleName]
importQualifierNamespaces imported = unLoc (ideclName imported)
  : map unLoc (maybeToList (ideclAs imported))

captureCompilerDefaultRecipe :: DynFlags -> String -> Either String CompilerDefaultRecipe
captureCompilerDefaultRecipe flags template =
  case T.breakOn (T.pack preambleDefaultDeclaration) (T.pack template) of
    (_, remaining) | T.null remaining ->
      if T.pack preambleDefaultMarker `T.isInfixOf` T.pack template
        then Left "compiler preamble default marker lacks its canonical default declaration"
        else do
          if T.pack preambleImportMarker `T.isInfixOf` T.pack template
            then () <$ replaceTemplateMarker preambleImportMarker preambleImportMarker template
            else Right ()
          Right NoCompilerDefault
    (before, remaining) -> do
      _ <- replaceTemplateMarker preambleImportMarker preambleImportMarker template
      _ <- replaceTemplateMarker preambleDefaultMarker preambleDefaultMarker template
      let after = T.drop (length preambleDefaultDeclaration) remaining
      if T.pack preambleDefaultDeclaration `T.isInfixOf` after
        then Left "compiler preamble repeats its default declaration"
        else do
          -- These placeholders belong to the trusted template, not authored
          -- imports. The latter remain native facts in SourcePrologue.
          let header = T.unpack (T.replace "{{CELL_PRAGMAS}}" ""
                (T.replace "{{CELL_IMPORTS}}" "" before))
          case unP parseHeader (initParserState
            (initParserOpts (xopt_set flags PackageImports))
            (stringToStringBuffer header)
            (mkRealSrcLoc (mkFastString "<compiler-preamble>") 1 1)) of
            PFailed _ -> Left "GHC could not parse the compiler preamble imports"
            POk _ parsed -> Right (PrimitiveCompilerDefault
              (map unLoc (maybeToList (hsmodName (unLoc parsed)))
                ++ concatMap (importQualifierNamespaces . unLoc) (hsmodImports (unLoc parsed))))

qualifyCompilerDefault :: CompilerDefaultRecipe -> [ModuleName] -> String -> Either String String
qualifyCompilerDefault recipe namespaces template = fst <$> qualifyCompilerDefaultWithLineOffset recipe namespaces template

qualifyCompilerDefaultWithLineOffset :: CompilerDefaultRecipe -> [ModuleName] -> String -> Either String (String,Int)
qualifyCompilerDefaultWithLineOffset recipe namespaces template = do
  (rendered, offset) <- qualifyCompilerDefaultRendering recipe namespaces (literalImportRendering template)
  pure (importRenderingSource rendered, offset)

qualifyCompilerDefaultRendering :: CompilerDefaultRecipe -> [ModuleName] -> ImportRendering
  -> Either String (ImportRendering,Int)
qualifyCompilerDefaultRendering NoCompilerDefault _ template = Right (template,0)
qualifyCompilerDefaultRendering (PrimitiveCompilerDefault templateNamespaces) importedNamespaces template = do
  let occupied = Set.fromList (templateNamespaces ++ importedNamespaces)
      fresh candidate namespaces
        | candidate `Set.member` namespaces = fresh (mkModuleName (moduleNameString candidate ++ "X")) namespaces
        | otherwise = candidate
      intAlias = fresh (mkModuleName "TidepoolCompilerDefaultInt") occupied
      doubleAlias = fresh (mkModuleName "TidepoolCompilerDefaultDouble") (Set.insert intAlias occupied)
      textAlias = fresh (mkModuleName "TidepoolCompilerDefaultText") (Set.insert doubleAlias (Set.insert intAlias occupied))
      primitive alias name = "import qualified " ++ show (unitString (moduleUnit (nameModule name)))
        ++ " " ++ moduleNameString (moduleName (nameModule name)) ++ " as " ++ moduleNameString alias ++ "\n"
      qualified alias name = moduleNameString alias ++ "." ++ occNameString (nameOccName name)
      imports = foldl appendImportRendering (literalImportRendering "")
        (map (locatedImportRendering ProtectedCompiler)
          [primitive intAlias intTyConName, primitive doubleAlias doubleTyConName
          ,"import qualified \"text\" Data.Text as " ++ moduleNameString textAlias ++ "\n"])
      replacement = appendImportRendering imports (literalImportRendering
        (preambleImportMarker ++ preambleDefaultMarker ++ "default (" ++ qualified intAlias intTyConName ++ ", " ++ qualified doubleAlias doubleTyConName
        ++ ", " ++ moduleNameString textAlias ++ ".Text)\n"))
  prepared <- replaceImportRendering preambleDefaultDeclaration replacement template
  let prefix = "{-# LANGUAGE PackageImports #-}\n"
  pure (appendImportRendering (literalImportRendering prefix) prepared, length (filter (== '\n') prefix))

-- The header transition owns both import placement and default qualification.
-- Body rendering cannot qualify the default again or move imports past it.
data PreparedDeclarationTemplate = PreparedDeclarationTemplate ImportRendering Int

preparedDeclarationImportOffset :: PreparedDeclarationTemplate -> Int
preparedDeclarationImportOffset (PreparedDeclarationTemplate _ offset) = offset

prepareDeclarationTemplate
  :: CompilerDefaultRecipe -> [ModuleName] -> String -> String
  -> Either String PreparedDeclarationTemplate
prepareDeclarationTemplate recipe namespaces imports template
  = prepareDeclarationTemplateWithImports recipe namespaces (literalImportRendering imports) template

prepareDeclarationTemplateWithImports
  :: CompilerDefaultRecipe -> [ModuleName] -> ImportRendering -> String
  -> Either String PreparedDeclarationTemplate
prepareDeclarationTemplateWithImports recipe namespaces imports source
  | T.pack "{{CELL_IMPORTS}}" `T.isInfixOf` T.pack source = do
      withImports <- replaceImportRendering "{{CELL_IMPORTS}}" imports template
      uncurry PreparedDeclarationTemplate <$> qualifyCompilerDefaultRendering recipe namespaces withImports
  | otherwise = case recipe of
      PrimitiveCompilerDefault _ -> do
        withImports <- replaceImportRendering preambleImportMarker
          (appendImportRendering imports (literalImportRendering preambleImportMarker)) template
        uncurry PreparedDeclarationTemplate <$> qualifyCompilerDefaultRendering recipe namespaces withImports
      NoCompilerDefault
        | null (importRenderingSource imports) -> Right (PreparedDeclarationTemplate template 0)
        | T.pack preambleImportMarker `T.isInfixOf` T.pack source ->
            (\rendered -> PreparedDeclarationTemplate rendered 0) <$> replaceImportRendering preambleImportMarker
              (appendImportRendering imports (literalImportRendering preambleImportMarker)) template
        | otherwise -> Left "declaration template with imports lacks {{CELL_IMPORTS}} or a protected preamble import marker"
  where template = literalImportRendering source


renderPreparedDeclaration :: PreparedDeclarationTemplate -> String -> String
renderPreparedDeclaration template body = importRenderingSource (renderPreparedDeclarationWithImports template body)

renderPreparedDeclarationWithImports :: PreparedDeclarationTemplate -> String -> ImportRendering
renderPreparedDeclarationWithImports (PreparedDeclarationTemplate (ImportRendering template origins) _) body =
  -- Declaration templates put authored declarations after their import header;
  -- source matching later checks every retained physical import range.
  ImportRendering (spliceTemplate template body "") origins

-- | Give generated turn scaffolds a stable owner name. The length-prefixed
-- UTF-8 fields make the identity independent of concatenation boundaries and
-- keep the digest domain separate from other hashes in the worker.
generatedScaffoldModuleName :: [String] -> String
generatedScaffoldModuleName fields = "TidepoolScaffold_" ++ shaHex framed
  where
    framed = BS.concat (frame "tidepool-generated-scaffold-owner-v1" : map frame fields)
    frame value =
      let bytes = TE.encodeUtf8 (T.pack value)
      in TE.encodeUtf8 (T.pack (show (BS.length bytes) ++ ":")) <> bytes

-- | Rename only a generated template's module declaration, before authored
-- turn text is inserted. The protected scaffold must have one conventional
-- module header; malformed or ambiguous templates fail closed.
renameScaffoldModuleHeader :: DynFlags -> String -> String -> Either String String
renameScaffoldModuleHeader flags replacement template = do
  let blank marker = T.replace (T.pack marker) (T.replicate (length marker) (T.singleton ' '))
      parserSource = T.unpack (blank "{{CELL_PRAGMAS}}" (blank "{{CELL_IMPORTS}}" (T.pack template)))
  declared <- parsedModuleHeaderName flags parserSource
  (oldName, nameSpan) <- maybe (Left "generated scaffold has no module header") Right declared
  nameOffset <- case getBufSpan nameSpan of
    Strict.Just span' -> Right (bufPos (bufSpanStart span'))
    Strict.Nothing -> Left "generated scaffold module name lacks a physical source location"
  let old = moduleNameString oldName
      prefix = "module "
      marker = prefix ++ old ++ " where"
      start = nameOffset - length prefix
  -- LINE pragmas remap logical locations. GHC BufPos counts physical decoded
  -- characters, so match it against String rather than UTF-8 byte offsets.
  if start < 0 || not (marker `isPrefixOf` drop start template)
    then Left "generated scaffold requires a conventional parsed module header"
    else replaceTemplateMarker marker ("module " ++ replacement ++ " where") template

-- | Replace one protected marker before authored text is inserted. Duplicate
-- or missing markers cannot silently alter the selected compiler recipe.
replaceTemplateMarker :: String -> String -> String -> Either String String
replaceTemplateMarker marker replacement template =
  let (before,after) = T.breakOn (T.pack marker) (T.pack template)
      remaining = T.drop (length marker) after
  in if null marker || T.null after || T.pack marker `T.isInfixOf` remaining
    then Left "checked recipe marker is missing or duplicated"
    else Right (T.unpack before ++ replacement ++ T.unpack remaining)

-- | Substitute the raw turn, statement-position turn, and binder list without
-- rescanning inserted text.
spliceTemplate :: String -> String -> String -> String
spliceTemplate template turnText binders = go template
  where
    go source
      | "{{TURN_STMT}}" `isPrefixOf` source = placeTurnStmt turnText ++ go (drop 13 source)
      | "{{TURN}}" `isPrefixOf` source = turnText ++ go (drop 8 source)
      | "{{BINDERS}}" `isPrefixOf` source = binders ++ go (drop 11 source)
    go (char : rest) = char : go rest
    go [] = []

-- | Place a turn in a @do@ block. A layout-sensitive @let@ statement gets
-- explicit braces; every result ends with a newline.
placeTurnStmt :: String -> String
placeTurnStmt turnText = case letRest of
  Just rest | not ("{" `isPrefixOf` dropWhile isSpace rest) ->
    let body = explicitBraceLetBody rest
     in "let {" ++ body ++ trailingNewline body ++ " }\n"
  _ -> turnText ++ trailingNewline turnText
  where
    trimmed = dropWhile isSpace turnText
    letRest = case stripPrefix "let" trimmed of
      Just rest@(char : _) | isSpace char -> Just rest
      _ -> Nothing
    trailingNewline text = if "\n" `isSuffixOf` text then "" else "\n"

-- | Mirror Rust's @explicit_brace_let_body@: preserve the separators that
-- layout inserts between declarations when a statement-position @let@ is
-- placed inside explicit braces. A deeper line continues the preceding
-- declaration's expression.
explicitBraceLetBody :: String -> String
explicitBraceLetBody rest = firstLine ++ go following
  where
    (firstLine, following) = break (== '\n') rest
    firstColumn = 4 + length (takeWhile isSpace firstLine)

    go [] = []
    go ('\n' : remaining) =
      let (line, followingLines) = break (== '\n') remaining
          (indent, content) = span isSpace line
          placed = if not (null content) && length indent + 1 == firstColumn
            then indent ++ ";" ++ content
            else line
       in '\n' : placed ++ go followingLines
    go other = other

-- | Parse an unpreprocessed generated source header with the request's parser
-- flags. Absence of an explicit header is distinct from malformed syntax.
-- General source compilation consumes ModSummary instead, including CPP.
extractModuleName :: DynFlags -> String -> Either String (Maybe String)
extractModuleName flags source =
  fmap (moduleNameString . fst) <$> parsedModuleHeaderName flags source

parsedModuleHeaderName :: DynFlags -> String -> Either String (Maybe (ModuleName, SrcSpan))
parsedModuleHeaderName flags source =
  case unP parseHeader (initParserState (initParserOpts flags)
      (stringToStringBuffer source) generatedHeaderStart) of
    PFailed _ -> failure
    POk state parsed
      | not (isEmptyMessages (getPsErrorMessages state)) -> failure
      | otherwise -> Right (fmap (\name -> (unLoc name, getLocA name)) (hsmodName (unLoc parsed)))
  where
    failure = Left "GHC could not parse the generated module header"

generatedHeaderStart :: RealSrcLoc
generatedHeaderStart = mkRealSrcLoc (mkFastString "<generated-module>") 1 1

renderImportBinder :: String -> String
renderImportBinder name
  | isSymOcc (mkVarOcc name) = "(" ++ name ++ ")"
  | otherwise = name
