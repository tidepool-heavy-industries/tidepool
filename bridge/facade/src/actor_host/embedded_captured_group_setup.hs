data GroupBox mode = GroupBox
  { groupState :: mode :- State (Maybe ForkGroupHandle)
  , storeGroup :: mode :- Call ForkGroupHandle NoReply
  , readGroup :: mode :- Call () (R.Reply (Maybe ForkGroupHandle))
  } deriving Generic
let groupBox = R.definition "embedded-checkpoint-groups" Actor.ReadOnly GroupBox
      { groupState = Nothing
      , storeGroup = \group -> R.put (Just group)
      , readGroup = \() -> R.get
      }
groupStore <- R.start groupBox
