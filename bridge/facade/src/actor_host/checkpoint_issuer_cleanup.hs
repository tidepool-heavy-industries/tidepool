do
  state <- pollResponse producer
  plan <- planCleanupFor producer
  receipt <- executeCleanup plan
  display (null (cleanupPlanPendingResponses plan)
    && null (cleanupPlanPendingWatches plan)
    && cleanupReceiptComplete receipt
    && case state of
      ResponseReady result -> responseValue result == "captured"
      _ -> False)
