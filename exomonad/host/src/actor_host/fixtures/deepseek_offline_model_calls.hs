import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Model as Model

offlineExplicitReasoning <- Model.invokeModel
  (Model.withReasoning (Model.DeepSeekReasoning Model.DeepSeekHigh)
    (Model.textTurn A.defaultSpec "deepseek-offline-bounded-explicit-high"))
  "deepseek-offline-bounded-explicit-high"
offlineInheritedReasoning <- Model.invokeModel
  (Model.textTurn A.defaultSpec "deepseek-offline-bounded-inherited")
  "deepseek-offline-bounded-inherited"
case (Model.modelOutcome offlineExplicitReasoning, Model.modelOutcome offlineInheritedReasoning) of
  (Right explicit, Right inherited) -> display (explicit == "bounded-high" && inherited == "bounded-inherited")
  _ -> display False
