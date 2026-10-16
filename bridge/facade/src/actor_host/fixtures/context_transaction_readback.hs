transactionFinal <- C.inspectContextTransaction transactionHandle
transactionCurrent <- C.getContext
transactionOriginal <- C.inspectOriginalText (C.targetReference transactionTarget) (C.targetSelector transactionTarget)
display (case transactionFinal of
  C.ContextTransactionCommitted receipt -> C.contextReceiptChanged receipt && length (C.contextReceiptAppended receipt) == 1
  _ -> False)
display (any ((== "conditional-context-replacement") . C.targetText) (C.textTargets transactionCurrent))
display (case transactionOriginal of C.OriginalTextAvailable text -> text == "parent-original"; _ -> False)
display (case filter (\metadata -> C.contextAuthorshipReference metadata == C.targetReference transactionTarget
        && C.contextAuthorshipSelector metadata == C.targetSelector transactionTarget)
        (view C.contextAuthorship transactionCurrent) of
  [metadata] -> C.contextAuthorshipAuthor metadata == C.ContextAuthorUser && not (C.contextAuthorshipNote metadata)
  _ -> False)
display (case transactionFinal of
  C.ContextTransactionCommitted receipt -> case C.contextReceiptAppended receipt of
    [reference] -> case filter (\metadata -> C.contextAuthorshipReference metadata == reference)
        (view C.contextAuthorship transactionCurrent) of
      [metadata] -> C.contextAuthorshipAuthor metadata == C.ContextAuthorAssistant && C.contextAuthorshipNote metadata
      _ -> False
    _ -> False
  _ -> False)
