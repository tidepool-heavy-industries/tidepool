import Tidepool.Inspection (Display (..))
data BrokenDisplay = BrokenDisplay { savedResult :: Cmd.RunResult }
instance Display BrokenDisplay where displayTree _ = error "deliberate display failure"
broken <- pure (BrokenDisplay finished)
broken
