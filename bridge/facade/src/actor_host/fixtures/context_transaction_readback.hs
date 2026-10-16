transactionFinal <- C.inspectContextTransaction transactionHandle
transactionCurrent <- C.getContext
transactionOriginal <- C.inspectOriginalText (C.targetReference transactionTarget) (C.targetSelector transactionTarget)
display (case transactionFinal of
  C.ContextTransactionCommitted receipt -> C.contextReceiptChanged receipt && length (C.contextReceiptAppended receipt) == 1
  _ -> False)
display (any ((== "conditional-context-replacement") . C.targetText) (C.textTargets transactionCurrent))
display (case transactionOriginal of C.OriginalTextAvailable text -> text == "parent-original"; _ -> False)
