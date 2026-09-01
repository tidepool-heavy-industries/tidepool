data CaptureError
  = CaptureExec ExecError
  | CaptureFs FsError
  deriving (Show, Eq)

captureCommand
  :: ( Member Exec effects
     , Member FsWrite effects
     , Member FsRead effects
     )
  => Text
  -> FilePath
  -> Eff effects (Either CaptureError Text)
captureCommand command path = do
  commandResult <- run command
  case commandResult of
    Left err -> pure (Left (CaptureExec err))
    Right process -> do
      writeResult <- writeFile path process.stdout
      case writeResult of
        Left err -> pure (Left (CaptureFs err))
        Right () -> first CaptureFs <$> readFile path
