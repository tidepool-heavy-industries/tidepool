import qualified RecordModelFixture as Probe
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Model as Model
recordProbe <- R.start (Probe.probe currentModel True)
(recordFirst, recordFirstLimit) <- R.call (Probe.runModel (R.client recordProbe)) "first"
display ((case (Model.modelOutcome recordFirst, Model.modelReceipt recordFirst) of { (Right (Probe.Answer 42), Just receipt) -> Model.providerRequests (Model.invocationUsage receipt) == 2; _ -> False }) && case Model.modelOutcome recordFirstLimit of { Left (Model.ModelBudgetExceeded Model.ProviderRequests) -> True; _ -> False })
