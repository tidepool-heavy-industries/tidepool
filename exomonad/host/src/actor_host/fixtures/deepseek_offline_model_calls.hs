import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Model as Model

let offlineModelLimits = Model.defaultLimits
      { Model.requestLimit = Just 1, Model.toolLimit = Just 0 }
offlineExplicitReasoning <- A.raiseSync (Model.invokeModel
  (Model.withLimits offlineModelLimits
    (Model.withReasoning (Model.DeepSeekReasoning Model.DeepSeekHigh)
      (Model.textTurn A.defaultSpec "deepseek-offline-bounded-explicit-high")))
  "deepseek-offline-bounded-explicit-high")
offlineInheritedReasoning <- A.raiseSync (Model.invokeModel
  (Model.withLimits offlineModelLimits
    (Model.textTurn A.defaultSpec "deepseek-offline-bounded-inherited"))
  "deepseek-offline-bounded-inherited")
case (Model.modelOutcome offlineExplicitReasoning, Model.modelOutcome offlineInheritedReasoning) of
  (Right explicit, Right inherited) -> display (explicit == "bounded-high" && inherited == "bounded-inherited")
  _ -> display False
