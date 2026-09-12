import GHC.Generics (Generic)
data ResultSink mode = ResultSink { sinkState :: mode :- State Int, acceptResult :: mode :- Call (Either ResponseFailure (ResponseResult Text)) NoReply, acceptedCount :: mode :- Call () (R.Reply Int) } deriving Generic
let sinkDefinition = coordinationActor "result-sink" ResultSink { sinkState = 0, acceptResult = \_ -> modify' (+1), acceptedCount = \() -> get }
sink <- R.start sinkDefinition
let oldDestination = acceptResult (R.client sink)
sink <- R.replace sink sinkDefinition
forwarding <- R.forwardResult producer oldDestination
