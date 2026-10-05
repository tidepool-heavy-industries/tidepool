import qualified Tidepool.Agent.Reply as Reply
do
  plan <- planCleanupFor producer
  receipt <- executeCleanup plan
  display (cleanupPlanPendingResponses plan == [Reply.requestIdNumber (requestId producer)]
    && null (cleanupPlanPendingWatches plan)
    && case (cleanupPlanRefusal plan, cleanupReceiptSteps receipt) of
      (Just _, [CleanupBlocked _]) -> not (cleanupReceiptComplete receipt)
      _ -> False)
