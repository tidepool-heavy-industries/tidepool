data BrokenDisplay = BrokenDisplay { savedResult :: Cmd.RunResult }
instance Show BrokenDisplay where show _ = error "deliberate display failure"
let broken = BrokenDisplay finished
broken
