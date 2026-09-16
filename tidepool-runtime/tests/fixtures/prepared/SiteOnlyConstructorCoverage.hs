data SiteOnlyAnswer
  = SiteOnlyChosen
  | SiteOnlyNeverMatched
  deriving (Show)

siteOnlyConstructorCoverage :: M Int
siteOnlyConstructorCoverage = do
  _ <- runLLMTurn @SiteOnlyAnswer "constructor-coverage"
  pure (42 :: Int)
