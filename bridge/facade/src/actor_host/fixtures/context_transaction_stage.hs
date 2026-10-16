import qualified Tidepool.Agent.Context as C

transactionBefore <- C.getContext
transactionTarget <- case filter ((== "parent-original") . C.targetText) (C.textTargets transactionBefore) of
  [target] -> pure target
  _ -> error "expected one exact original field"
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
transactionStillCommitted <- C.getContext
display (any ((== "parent-original") . C.targetText) (C.textTargets transactionStillCommitted))
