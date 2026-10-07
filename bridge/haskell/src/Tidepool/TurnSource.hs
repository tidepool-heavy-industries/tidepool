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
  , preambleDefaultDeclaration, preambleImportMarker, importQualifierNamespaces
  , PreparedDeclarationTemplate, prepareDeclarationTemplate, renderPreparedDeclaration
  ) where

import GHC (GhcPs, ImportDecl(..), ModuleName, hsmodImports, hsmodName, unLoc, moduleNameString, mkModuleName)
import GHC.Builtin.Types (intTyConName, doubleTyConName)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Driver.Session (DynFlags, xopt_set)
import GHC.LanguageExtensions (Extension(PackageImports))
import GHC.Parser (parseHeader)
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Data.FastString (mkFastString)
import GHC.Types.SrcLoc (mkRealSrcLoc)
import GHC.Types.Name (nameModule, nameOccName)
import GHC.Unit.Types (moduleUnit, moduleName, unitString)
import GHC.Types.Name.Occurrence (isSymOcc, mkVarOcc, occNameString)
import Data.Char (isAlphaNum, isSpace)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix)
import Data.Maybe (listToMaybe, maybeToList)
import qualified Data.Set as Set
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.ByteString as BS
import Tidepool.ExtractUtil (shaHex)

-- This is the shared Rust/Haskell preamble ABI marker. Import insertion
-- consumes it before the compiler qualifies the protected default declaration.
preambleDefaultDeclaration :: String
preambleDefaultDeclaration = preambleImportMarker ++ "default (Int, Double, Text)\n"

preambleImportMarker :: String
preambleImportMarker = "-- tidepool-preamble-imports-v1\n"

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
      if T.pack preambleImportMarker `T.isInfixOf` T.pack template
        then Left "compiler preamble import marker lacks its canonical default declaration"
        else Right NoCompilerDefault
    (before, remaining) -> do
      _ <- replaceTemplateMarker preambleImportMarker preambleImportMarker template
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
qualifyCompilerDefaultWithLineOffset NoCompilerDefault _ template = Right (template,0)
qualifyCompilerDefaultWithLineOffset (PrimitiveCompilerDefault templateNamespaces) importedNamespaces template = do
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
      replacement = primitive intAlias intTyConName ++ primitive doubleAlias doubleTyConName
        ++ "import qualified \"text\" Data.Text as " ++ moduleNameString textAlias ++ "\n"
        ++ preambleImportMarker ++ "default (" ++ qualified intAlias intTyConName ++ ", " ++ qualified doubleAlias doubleTyConName
        ++ ", " ++ moduleNameString textAlias ++ ".Text)\n"
  prepared <- replaceTemplateMarker preambleDefaultDeclaration replacement template
  let prefix = "{-# LANGUAGE PackageImports #-}\n"
  pure (prefix ++ prepared, length (filter (== '\n') prefix))

-- The header transition owns both import placement and default qualification.
-- Body rendering cannot qualify the default again or move imports past it.
newtype PreparedDeclarationTemplate = PreparedDeclarationTemplate String

prepareDeclarationTemplate
  :: CompilerDefaultRecipe -> [ModuleName] -> String -> String
  -> Either String PreparedDeclarationTemplate
prepareDeclarationTemplate recipe namespaces imports template
  | T.pack "{{CELL_IMPORTS}}" `T.isInfixOf` T.pack template = do
      withImports <- replaceTemplateMarker "{{CELL_IMPORTS}}" imports template
      PreparedDeclarationTemplate <$> qualifyCompilerDefault recipe namespaces withImports
  | otherwise = case recipe of
      PrimitiveCompilerDefault _ -> do
        withImports <- replaceTemplateMarker preambleImportMarker
          (imports ++ preambleImportMarker) template
        PreparedDeclarationTemplate <$> qualifyCompilerDefault recipe namespaces withImports
      NoCompilerDefault
        | null imports -> Right (PreparedDeclarationTemplate template)
        | otherwise -> Left "declaration template with imports lacks {{CELL_IMPORTS}} or a protected preamble import marker"

renderPreparedDeclaration :: PreparedDeclarationTemplate -> String -> String
renderPreparedDeclaration (PreparedDeclarationTemplate template) body =
  spliceTemplate template body ""

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
renameScaffoldModuleHeader :: String -> String -> Either String String
renameScaffoldModuleHeader replacement template = do
  old <- maybe (Left "generated scaffold has no module header") Right (extractModuleName template)
  replaceTemplateMarker ("module " ++ old ++ " where")
    ("module " ++ replacement ++ " where") template

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

-- | Read the module name from a conventional module header.
extractModuleName :: String -> Maybe String
extractModuleName source = listToMaybe
  [ name
  | line <- lines source
  , Just rest <- [stripPrefix "module " (dropWhile (== ' ') line)]
  , let name = takeWhile (\char -> isAlphaNum char || char == '.' || char == '_')
          (dropWhile (== ' ') rest)
  , not (null name)
  ]

renderImportBinder :: String -> String
renderImportBinder name
  | isSymOcc (mkVarOcc name) = "(" ++ name ++ ")"
  | otherwise = name
