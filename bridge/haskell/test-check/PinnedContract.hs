{-# LANGUAGE DataKinds, GADTs, OverloadedStrings, TypeApplications #-}
module Main where

import Control.Exception (Exception, evaluate, throwIO, try)
import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, runM, sendM)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Data.Text.IO as Text
import qualified Project.Checks as Pinned
import System.Environment (getArgs)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import Tidepool.Effects.Core (RecipeCheck (..))

data Captured = Captured Text deriving (Show)
instance Exception Captured

handler :: RecipeCheck a -> Eff '[IO] a
handler request = case request of
  RecipeRoot -> pure ("pinned", 1, 1)
  RecipeTurn _ source -> sendM (throwIO (Captured source))
  _ -> error "unexpected effect before pinned assertion source capture"

main :: IO ()
main = do
  [support, scratch] <- getArgs
  captured <- try @Captured (runM (interpret handler Pinned.pinned))
  source <- case captured of
    Left (Captured cell) -> pure cell
    Right _ -> error "pinned fixture did not submit its assertion cell"
  let (imports, expression) = span (Text.isPrefixOf "import ") (Text.lines source)
      moduleSource = Text.unlines $
        [ "{-# LANGUAGE DataKinds, OverloadedStrings #-}"
        , "module Main where"
        , "import Control.Exception (evaluate)"
        , "import Control.Monad.Freer (Eff, run)"
        , "import Ext.Tiny"
        ] ++ imports ++
        [ "main :: IO ()"
        , "main = evaluate (run (cell :: Eff '[] ()))"
        , "cell ="
        ] ++ map ("  " <>) expression
      path = scratch ++ "/PinnedCell.hs"
      binary = scratch ++ "/pinned-cell"
  Text.writeFile (scratch ++ "/actual-cell.hs") source
  Text.writeFile path moduleSource
  (compiled, out, err) <- readProcessWithExitCode "ghc"
    [ "-O0", "-i" ++ support, "-i" ++ scratch
    , "-ibridge/haskell/lib", "-ibridge/haskell/actors"
    , "-outputdir", scratch ++ "/cell-objects", "-o", binary, path
    ] ""
  writeFile (scratch ++ "/cell-build.log") (out ++ err)
  unless (compiled == ExitSuccess) (error "pinned actor cell failed native compilation")
  (completed, stdoutText, stderrText) <- readProcessWithExitCode binary [] ""
  writeFile (scratch ++ "/cell-execution.log") (stdoutText ++ stderrText)
  evaluate completed >>= \status -> unless (status == ExitSuccess) (error "pinned actor assertion failed")
  putStrLn "passed: actual pinned fixture emits a typed assertion evaluated with external Ext.Tiny (tiny = 41)"
  putStrLn "executed: 1 native pinned-source contract; not executed: facade flake capture and resident host integration"
