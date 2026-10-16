import qualified Tidepool.Agent.Context as C
import Control.Lens (view)

transactionBefore <- C.getContext
transactionTarget <- case filter ((== "parent-original") . C.targetText) (C.textTargets transactionBefore) of
  [target] -> pure target
  _ -> error "expected one exact original field"
display (case filter (\metadata -> C.contextAuthorshipReference metadata == C.targetReference transactionTarget
        && C.contextAuthorshipSelector metadata == C.targetSelector transactionTarget)
        (view C.contextAuthorship transactionBefore) of
  [metadata] -> C.contextAuthorshipAuthor metadata == C.ContextAuthorUser && not (C.contextAuthorshipNote metadata)
  _ -> False)
let transactionPatch = C.contextPatch
      [ C.replaceText transactionTarget "conditional-context-replacement"
      , C.appendNote "assistant-source-note" [C.targetReference transactionTarget]
      ]
transactionPreview <- case C.previewContext transactionBefore transactionPatch of
  Right preview -> pure preview
  Left problems -> error (T.pack (show problems))
C.ContextStaged transactionHandle <- C.stageContext transactionPatch
transactionLocal <- C.inspectContextTransaction transactionHandle
display (case transactionLocal of C.ContextTransactionStaged -> True; _ -> False)
transactionRepeated <- C.stageContext transactionPatch
display (case transactionRepeated of C.ContextStaged repeated -> repeated == transactionHandle; _ -> False)
transactionSecond <- C.stageContext (C.contextPatch [C.appendNote "must-refuse-second-batch" []])
display (case transactionSecond of
  C.ContextStageRefused problems -> map C.contextProblemCode problems == [C.ContextProblemAlreadyStaged]
  _ -> False)
transactionStillCommitted <- C.getContext
display (any ((== "parent-original") . C.targetText) (C.textTargets transactionStillCommitted))
