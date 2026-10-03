module Tidepool.CheckedRecipe
  ( checkedDisplayRecipe, checkedProgramDisplayRecipe, writeCheckedDisplayReceipt
  , checkedItemCompilePurpose, checkedRecipeSource, replaceRecipeMarker, writeCheckedItemReceipt
  ) where

import Codec.CBOR.Encoding (encodeBytes, encodeListLen, encodeString, encodeWord64)
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (unless, when)
import qualified Data.ByteString as BS
import Data.List (intercalate, isInfixOf, stripPrefix)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import GHC (moduleUnit, moduleName, moduleNameString)
import GHC.Builtin.Types (intTyConName)
import GHC.Types.Name (nameModule)
import GHC.Unit.Types (unitString)
import System.FilePath ((</>))
import Tidepool.CheckedAdmission (checkedDisplayBinders)
import Tidepool.CheckedCell (CheckedSignature(..))
import Tidepool.ExactScope
  ( ExactScope(..), CheckedItemAdmission(..), CheckedItemPurpose(..), CheckedDisplayAdmission(..) )
import Tidepool.ExtractUtil (shaHex)
import Tidepool.GhcPipeline (CompilePurpose(..))
import Tidepool.TurnSource (spliceTemplate, replaceTemplateMarker, renderImportBinder)

checkedDisplayRecipe :: CheckedDisplayAdmission -> String -> IO String
checkedDisplayRecipe = checkedDisplayRecipeWithInputs False

checkedProgramDisplayRecipe :: CheckedDisplayAdmission -> String -> IO String
checkedProgramDisplayRecipe = checkedDisplayRecipeWithInputs True

checkedDisplayRecipeWithInputs :: Bool -> CheckedDisplayAdmission -> String -> IO String
checkedDisplayRecipeWithInputs generic admission template = do
  let rowPrefix = "{{TURN_STMT}} ; _ <- (pure () :: Eff "
      rows = [suffix | line <- lines template, Just suffix <- [stripPrefix rowPrefix line]]
  effectRow <- case rows of
    [suffix] -> do
      case T.stripSuffix " ())" (T.pack suffix) of
        Just row | not (T.null row) -> pure (T.unpack row)
        _ -> fail "display requires the canonical bind effect-row pin"
    _ -> fail "display requires one exact bind effect-row pin"
  withImports <- replaceRecipeMarker "default (Int, Double, Text)\n"
    ("import qualified Tidepool.Inspection as TidepoolInspection\n"
      ++ (if generic then "import qualified " ++ show (unitString (moduleUnit (nameModule intTyConName)))
        ++ " " ++ moduleNameString (moduleName (nameModule intTyConName))
        ++ " as TidepoolProgramTypes\nimport qualified \"text\" Data.Text as TidepoolProgramText\n" else "")
      ++ concatMap (\(name, binders) -> "import " ++ name ++ " (" ++ intercalate ", " (map renderImportBinder binders) ++ ")\n")
        (displayValueImports admission) ++ "default (Int, Double, Text)\n") template
  unless ("__result = do {\n{{TURN_STMT}}" `isInfixOf` withImports)
    (fail "display requires canonical bind recipe version one")
  (page,metadata,alias) <- case checkedDisplayBinders admission of
    [page,metadata,alias] -> pure (page,metadata,alias)
    _ -> fail "display requires its three canonical binders"
  let keys = intercalate "," ["T.pack " ++ show key | key <- displayPresented admission]
      budget = if generic then "(__tidepoolBudget :: TidepoolProgramTypes.Int)" else show (displayBudget admission)
      presented = if generic then "(__tidepoolPresented :: [TidepoolProgramText.Text])" else "[" ++ keys ++ "]"
      rendering = if displayPresentation admission == "rendered"
        then "TidepoolInspection.displayPageWithout " ++ presented ++ " " ++ budget
          ++ " (" ++ displayObservationName admission ++ " ())"
        else "TidepoolInspection.pageWithContinuation " ++ budget
          ++ " (TidepoolInspection.TextLeaf (T.pack \"<opaque value>\")) Nothing"
      statement = "(" ++ intercalate ", " [page,metadata,alias] ++ ") <- do {\n"
        ++ page ++ " <- pure ((" ++ rendering ++ ") :: TidepoolInspection.DisplayPage " ++ effectRow ++ ");\n"
        ++ metadata ++ " <- pure (T.copy (TidepoolInspection.text " ++ page ++ "), TidepoolInspection.pageHasMore "
        ++ page ++ ", TidepoolInspection.pageUnavailable " ++ page ++ ");\n"
        ++ alias ++ " <- pure " ++ page ++ ";\npure (" ++ intercalate ", " [page,metadata,alias] ++ ")\n}"
  let spliced = (if generic then ("{-# LANGUAGE PackageImports, ScopedTypeVariables #-}\n" ++) else id) (spliceTemplate withImports statement (intercalate ", " [page,metadata,alias]))
  if generic then do
    withArgument <- replaceRecipeMarker "__result = do {" "__result ((__tidepoolBudget :: TidepoolProgramTypes.Int), (__tidepoolPresented :: [TidepoolProgramText.Text])) = do {" spliced
    prepared <- replaceRecipeMarker "__prepared = TidepoolResume.settle __result" "__prepared input = TidepoolResume.settle (__result input)" withArgument
    pure prepared
  else pure spliced

writeCheckedDisplayReceipt :: FilePath -> ExactScope -> CheckedDisplayAdmission -> String -> IO ()
writeCheckedDisplayReceipt root scope admission source = do
  let text = encodeString . T.pack
      receipt = encodeListLen 8 <> text "TPEXACTDISPLAY" <> text "1"
        <> text (scopeRequestSha256 scope) <> text (displayCellReceiptDigest admission)
        <> encodeWord64 (displayItemIndex admission) <> text (displayPrefixDigest admission)
        <> text (shaHex (TE.encodeUtf8 (T.pack source))) <> text "tidepool-display-recipe-1"
  BS.writeFile (root </> "checked-display.cbor") (toStrictByteString receipt)

checkedRecipeAnnotations :: CheckedItemAdmission -> [(String,CheckedSignature)]
checkedRecipeAnnotations admission =
  [("__tidepool_checked_annotation_" ++ show index,signature)
  | (index,signature) <- zip [(0::Int)..] (itemSignatures admission)]

checkedItemCompilePurpose :: CheckedItemAdmission -> CompilePurpose
checkedItemCompilePurpose admission = case itemPurpose admission of
  AuthoredCheckedItem -> CheckedItemCompile annotations original values
  HostActivationInput -> HostActivationInputCompile annotations original values
  where
    annotations = checkedRecipeAnnotations admission
    original = itemPlannedDeclaration admission
    values = itemCompletedValues admission

-- The admitted template is a versioned recipe input. Transform its markers
-- before inserting authored bytes, so authored syntax is never rescanned.
checkedRecipeSource :: CheckedItemAdmission -> String -> String -> IO String
checkedRecipeSource admission template source = case itemKind admission of
  "bind" -> do
    unless ("__result = do {\n{{TURN_STMT}}" `isInfixOf` template)
      (fail "checked bind requires canonical recipe version one")
    let aliases = checkedRecipeAnnotations admission
        declarations = intercalate "; " [alias ++ " :: (); "
          ++ alias ++ " = " ++ binder | ((alias,_),binder) <- zip aliases (itemBinders admission)]
        result = intercalate ", " (map fst aliases)
    amended <- if null aliases then pure template else replaceRecipeMarker "{{TURN_STMT}}"
      ("{{TURN_STMT}}\n; let { " ++ declarations ++ " }\n") template
    pure (spliceTemplate amended source result)
  "expr" -> case checkedRecipeAnnotations admission of
    [(alias,_)] -> do
      observation <- maybe (fail "checked expression has no owning observation name") pure (itemObservationName admission)
      unless ("__result = do {\n{{TURN_STMT}}" `isInfixOf` template)
        (fail "checked expression capture requires canonical bind recipe version two")
      let liftStatement = case itemExpressionLift admission of
            Just "effectful" -> "__tidepool_checked_captured_value <- " ++ alias
              ++ "\n; let { " ++ observation ++ " = (\\() -> __tidepool_checked_captured_value) }"
            Just "pure" -> "let { " ++ observation ++ " = (\\() -> " ++ alias ++ ") }"
            _ -> ""
      when (null liftStatement) (fail "checked expression has no certified lift plan")
      let statement = "let { " ++ alias ++ " :: (); "
            ++ alias ++ " = (\n" ++ source ++ "\n) }\n; " ++ liftStatement
      pure (spliceTemplate template statement observation)
    _ -> fail "checked expression has no unique full signature"
  _ -> fail "checked declaration lacks an original identity certificate"

replaceRecipeMarker :: String -> String -> String -> IO String
replaceRecipeMarker marker replacement template =
  either fail pure (replaceTemplateMarker marker replacement template)

writeCheckedItemReceipt :: FilePath -> ExactScope -> CheckedItemAdmission -> String -> IO ()
writeCheckedItemReceipt root scope admission source = do
  let (file,magic,profile) = case itemPurpose admission of
        AuthoredCheckedItem -> ("checked-item.cbor","TPEXACTITEM","tidepool-checked-recipe-2")
        HostActivationInput -> ("activation-input.cbor","TPEXACTACTIVATIONINPUT2","tidepool-host-activation-input-2")
      text = encodeString . T.pack
      receipt = encodeListLen (if itemPurpose admission == HostActivationInput then 9 else 8) <> text magic
        <> text (if itemPurpose admission == HostActivationInput then "2" else "1")
        <> text (scopeRequestSha256 scope) <> text (itemAdmissionDigest admission)
        <> text (itemCellReceiptDigest admission) <> encodeWord64 (itemIndex admission)
        <> text (shaHex (TE.encodeUtf8 (T.pack source))) <> text profile
  witness <- case itemPurpose admission of
    AuthoredCheckedItem -> pure mempty
    HostActivationInput -> encodeBytes <$> BS.readFile (root </> "activation-type.cbor")
  BS.writeFile (root </> file) (toStrictByteString (receipt <> witness))
