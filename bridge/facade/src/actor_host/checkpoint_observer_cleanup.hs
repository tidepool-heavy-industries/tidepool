do
  state <- pollResponse observer
  plan <- planCleanupFor observer
  receipt <- executeCleanup plan
  display (null (cleanupPlanPendingResponses plan)
    && null (cleanupPlanPendingWatches plan)
    && cleanupReceiptComplete receipt
    && case state of
      ResponseReady result -> responseValue result == "inspected"
      _ -> False)
