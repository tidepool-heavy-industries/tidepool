module Tidepool.CheckedAdmission
  ( validateCheckedCellAdmission, validateCheckedItemAdmission
  , checkedDisplayBinders, validateCheckedDisplayAdmission
  ) where

import Control.Monad (forM, unless, when)
import qualified Data.ByteString as BS
import Data.List (isInfixOf)
import Data.Word (Word64)
import qualified Data.Map.Strict as Map
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Tidepool.Binders (StmtBinders(..), TurnKind(..))
import Tidepool.CheckedCell (CheckedSignature(..))
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExactScope
  ( CheckedCellAdmission(..), CheckedItemAdmission(..), CheckedDisplayAdmission(..) )
import Tidepool.ExtractRequest (WorkerRequest(..))
import Tidepool.ExtractUtil (shaHex)

-- Each checkpoint rereads the ordered recipe inventory, including duplicates.
-- A successful earlier admission does not authorize changed files later.
readTurnTemplateDigests :: WorkerRequest -> IO [(String, String)]
readTurnTemplateDigests args = forM (requestTurnTemplates args) $ \(kind,path) ->
  (,) kind . shaHex <$> BS.readFile path

requireGeneration :: WorkerRequest -> IO Word64
requireGeneration args = maybe (error "required argument missing: --bind-gen") pure
  (requestBindGen args)

validateCheckedCellAdmission :: WorkerRequest -> CheckedCellAdmission -> String -> String -> IO ()
validateCheckedCellAdmission args admission cellSource template = do
  templateDigests <- readTurnTemplateDigests args
  unless (shaHex (TE.encodeUtf8 (T.pack cellSource)) == checkedCellSha256 admission
      && shaHex (TE.encodeUtf8 (T.pack template)) == checkedTemplateSha256 admission
      && templateDigests == checkedTurnTemplates admission
      && requestInjectVals args == checkedInjectedModules admission)
    (fail "cell body, wrapper or injected interfaces differ from immutable admission")

validateCheckedItemAdmission :: WorkerRequest -> CheckedItemAdmission -> String -> StmtBinders -> IO ()
validateCheckedItemAdmission args admission source verdict = do
  templates <- readTurnTemplateDigests args
  generation <- requireGeneration args
  let expectedKind = case sbKind verdict of KBind -> "bind"; KExpr -> "expr"; KDecl -> "decl"
      keys = if expectedKind == "bind"
        then ["__tidepool_cell_pin_" ++ show (itemIndex admission) ++ "_" ++ binder | binder <- itemBinders admission]
        else ["__tidepool_cell_expr_" ++ show (itemIndex admission)]
  unless (shaHex (TE.encodeUtf8 (T.pack source)) == itemSourceDigest admission
      && expectedKind == itemKind admission && sbBinders verdict == itemBinders admission
      && generation == itemGeneration admission && requestInjectVals args == itemInjectedModules admission
      && templates == itemTurnTemplates admission && map signatureKey (itemSignatures admission) == keys)
    (fail "checked item body, verdict, generation, signatures or recipe differs from its protected offer")
  when ("__tidepool_checked_annotation_" `isInfixOf` source)
    (fail "authored checked item uses a compiler-reserved annotation name")

checkedDisplayBinders :: CheckedDisplayAdmission -> [String]
checkedDisplayBinders admission =
  ["__tidepoolPage" ++ show (displayGeneration admission)
  ,"__tidepoolMetadata" ++ show (displayGeneration admission),"cellDisplay"]

validateCheckedDisplayAdmission :: WorkerRequest -> CheckedDisplayAdmission -> String -> StmtBinders -> IO ()
validateCheckedDisplayAdmission args admission source verdict = do
  templates <- readTurnTemplateDigests args
  generation <- requireGeneration args
  unless (null source && sbKind verdict == KBind && sbBinders verdict == checkedDisplayBinders admission
      && generation == displayGeneration admission && templates == displayTurnTemplates admission
      && requestInjectVals args == displayInjectedModules admission
      && not (requestActivationPreview args))
    (fail "display request differs from its completed observation admission")
  let observation = SymbolIdentity "main"
        (T.pack ("Tidepool.Session.Val.G" ++ show (displayCaptureGeneration admission)))
        "value" (T.pack (displayObservationName admission)) Nothing
  unless (Map.lookup observation (requestRetainedGenerations args) == Just (displayCaptureGeneration admission))
    (fail "display lacks its exact retained observation generation")
