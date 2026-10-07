module Tidepool.CheckedRecipe
  ( checkedItemCompilePurpose, checkedRecipeSource, checkedRecipeSourceWithLineOffset
  , replaceRecipeMarker, writeCheckedItemReceipt
  ) where

import Codec.CBOR.Encoding (encodeListLen, encodeString, encodeWord64)
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (unless, when)
import qualified Data.ByteString as BS
import Data.List (intercalate, isInfixOf)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import GHC (ModuleName)
import System.FilePath ((</>))
import Tidepool.CheckedCell (CheckedSignature(..))
import Tidepool.ExactScope
  ( ExactScope(..), CheckedItemAdmission(..) )
import Tidepool.ExtractUtil (shaHex)
import Tidepool.GhcPipeline (CompilePurpose(..))
import Tidepool.TurnSource
  ( spliceTemplate, replaceTemplateMarker
  , CompilerDefaultRecipe, qualifyCompilerDefaultWithLineOffset )

checkedRecipeAnnotations :: CheckedItemAdmission -> [(String,CheckedSignature)]
checkedRecipeAnnotations admission =
  [("__tidepool_checked_annotation_" ++ show index,signature)
  | (index,signature) <- zip [(0::Int)..] (itemSignatures admission)]

checkedItemCompilePurpose :: CheckedItemAdmission -> CompilePurpose
checkedItemCompilePurpose admission = CheckedItemCompile annotations original values
  where
    annotations = checkedRecipeAnnotations admission
    original = itemPlannedDeclaration admission
    values = itemCompletedValues admission

-- The admitted template is a versioned recipe input. Transform its markers
-- before inserting authored bytes, so authored syntax is never rescanned.
checkedRecipeSource :: CompilerDefaultRecipe -> [ModuleName] -> CheckedItemAdmission -> String -> String -> IO String
checkedRecipeSource defaults namespaces admission template source =
  fst <$> checkedRecipeSourceWithLineOffset defaults namespaces admission template source

checkedRecipeSourceWithLineOffset :: CompilerDefaultRecipe -> [ModuleName] -> CheckedItemAdmission
  -> String -> String -> IO (String,Int)
checkedRecipeSourceWithLineOffset defaults namespaces admission template source = case itemKind admission of
  "bind" -> do
    unless ("__result = do {\n{{TURN_STMT}}" `isInfixOf` template)
      (fail "checked bind requires canonical recipe version one")
    let aliases = checkedRecipeAnnotations admission
        declarations = intercalate "; " [alias ++ " :: (); "
          ++ alias ++ " = " ++ binder | ((alias,_),binder) <- zip aliases (itemBinders admission)]
        result = intercalate ", " (map fst aliases)
    amended <- if null aliases then pure template else replaceRecipeMarker "{{TURN_STMT}}"
      ("{{TURN_STMT}}\n; let { " ++ declarations ++ " }\n") template
    (prepared,lineOffset) <- either fail pure (qualifyCompilerDefaultWithLineOffset defaults namespaces amended)
    pure (spliceTemplate prepared source result,lineOffset)
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
      (prepared,lineOffset) <- either fail pure (qualifyCompilerDefaultWithLineOffset defaults namespaces template)
      pure (spliceTemplate prepared statement observation,lineOffset)
    _ -> fail "checked expression has no unique full signature"
  _ -> fail "checked declaration lacks an original identity certificate"

replaceRecipeMarker :: String -> String -> String -> IO String
replaceRecipeMarker marker replacement template =
  either fail pure (replaceTemplateMarker marker replacement template)

writeCheckedItemReceipt :: FilePath -> ExactScope -> CheckedItemAdmission -> String -> IO ()
writeCheckedItemReceipt root scope admission source = do
  let text = encodeString . T.pack
      receipt = encodeListLen 8 <> text "TPEXACTITEM" <> text "1"
        <> text (scopeRequestSha256 scope) <> text (itemAdmissionDigest admission)
        <> text (itemCellReceiptDigest admission) <> encodeWord64 (itemIndex admission)
        <> text (shaHex (TE.encodeUtf8 (T.pack source))) <> text "tidepool-checked-recipe-2"
  BS.writeFile (root </> "checked-item.cbor") (toStrictByteString receipt)
