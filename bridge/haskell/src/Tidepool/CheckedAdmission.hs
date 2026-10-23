module Tidepool.CheckedAdmission
  ( matchesInspectionAdmission, validateCheckedCellAdmission, validateCheckedItemAdmission
  ) where

import Control.Monad (forM, unless, when)
import qualified Data.ByteString as BS
import Data.List (isInfixOf)
import Data.Maybe (isJust)
import Data.Word (Word64)
import qualified Data.Map.Strict as Map
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Tidepool.Binders (StmtBinders(..), TurnKind(..))
import Tidepool.CheckedCell (CheckedSignature(..))
import Tidepool.ExactScope
  ( CheckedCellAdmission(..), CheckedItemAdmission(..) )
import Tidepool.ExtractRequest (RequestOptions(..), InspectionPayload)
import Tidepool.ExtractUtil (shaHex)

-- An inspection input cap cannot be used by an executable or publication mode.
matchesInspectionAdmission :: RequestOptions -> InspectionPayload -> [String] -> Bool
matchesInspectionAdmission args _inspection values =
  not (optionHarnessProfile args)
    && not (isJust (optionBindGen args))
    && Map.null (optionRetainedGenerations args)
    && optionInjectVals args == values

-- Each checkpoint rereads the ordered recipe inventory, including duplicates.
-- A successful earlier admission does not authorize changed files later.
readTurnTemplateDigests :: [(String, FilePath)] -> IO [(String, String)]
readTurnTemplateDigests templates = forM templates $ \(kind,path) ->
  (,) kind . shaHex <$> BS.readFile path

requireGeneration :: RequestOptions -> IO Word64
requireGeneration args = maybe (error "required argument missing: --bind-gen") pure
  (optionBindGen args)

validateCheckedCellAdmission :: RequestOptions -> [(String, FilePath)] -> CheckedCellAdmission -> String -> String -> IO ()
validateCheckedCellAdmission args templates admission cellSource template = do
  templateDigests <- readTurnTemplateDigests templates
  unless (shaHex (TE.encodeUtf8 (T.pack cellSource)) == checkedCellSha256 admission
      && shaHex (TE.encodeUtf8 (T.pack template)) == checkedTemplateSha256 admission
      && templateDigests == checkedTurnTemplates admission
      && optionInjectVals args == checkedInjectedModules admission)
    (fail "cell body, wrapper or injected interfaces differ from immutable admission")

validateCheckedItemAdmission :: RequestOptions -> [(String, FilePath)] -> CheckedItemAdmission -> String -> StmtBinders -> IO ()
validateCheckedItemAdmission args recipes admission source verdict = do
  templates <- readTurnTemplateDigests recipes
  generation <- requireGeneration args
  let expectedKind = case sbKind verdict of KBind -> "bind"; KExpr -> "expr"; KDecl -> "decl"
      keys = if expectedKind == "bind"
        then ["__tidepool_cell_pin_" ++ show (itemIndex admission) ++ "_" ++ binder | binder <- itemBinders admission]
        else ["__tidepool_cell_expr_" ++ show (itemIndex admission)]
  unless (shaHex (TE.encodeUtf8 (T.pack source)) == itemSourceDigest admission
      && expectedKind == itemKind admission && sbBinders verdict == itemBinders admission
      && generation == itemGeneration admission && optionInjectVals args == itemInjectedModules admission
      && templates == itemTurnTemplates admission && map signatureKey (itemSignatures admission) == keys)
    (fail "checked item body, verdict, generation, signatures or recipe differs from its protected offer")
  when ("__tidepool_checked_annotation_" `isInfixOf` source)
    (fail "authored checked item uses a compiler-reserved annotation name")
