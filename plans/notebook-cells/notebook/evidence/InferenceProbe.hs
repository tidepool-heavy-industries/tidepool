{-# LANGUAGE ScopedTypeVariables #-}

import Control.Monad.IO.Class
import Data.Data
import GHC
import GHC.Types.Id
import GHC.Types.Name
import GHC.Utils.Outputable
import System.Environment

ids :: Data a => a -> [Id]
ids value =
  maybe [] (: []) (cast value)
    ++ concat (gmapQ ids value)

main :: IO ()
main = do
  [lib, path, moduleName, binder, includePath, effectsPath] <- getArgs
  runGhc (Just lib) $ do
    flags <- getSessionDynFlags
    _ <-
      setSessionDynFlags
        flags {importPaths = includePath : effectsPath : importPaths flags}
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName moduleName)
    parsed <- parseModule summary
    checked <- typecheckModule parsed
    liftIO $
      mapM_
        ( \identifier ->
            putStrLn
              ( renderWithContext
                  defaultSDocContext
                  (ppr identifier <+> text "::" <+> ppr (idType identifier))
              )
      )
        [ identifier
        | identifier <- ids (tm_typechecked_source checked),
          getOccString identifier == binder
        ]
