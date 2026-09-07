module Project.Types where

import Data.Text (Text)

-- Project language is ordinary source, independent of runtime authority.
data Task = Task
  { planPath :: Text
  , obligation :: Text
  , acceptance :: Text
  } deriving (Show, Eq)

data Candidate = Candidate
  { candidateCommit :: Text
  , checkedCommands :: [Text]
  , remainingGates :: [Text]
  } deriving (Show, Eq)

data Review = Accepted Candidate | Repair Candidate Text | NeedsDesign Text
  deriving (Show, Eq)

data Delivery = Integrated Text [Text] | Preparation Candidate | Blocked Text
  deriving (Show, Eq)
