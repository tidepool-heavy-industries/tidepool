{-# LANGUAGE DataKinds, QuasiQuotes #-}
{-# LANGUAGE FlexibleContexts, UndecidableInstances #-}
{{CELL_PRAGMAS}}
module CellCheck where
import Control.Monad.Freer (Eff)
import qualified Data.Text as PackageText
import qualified {{OPERAND_MODULE}} as TidepoolHostInput
{{CELL_IMPORTS}}
__tidepoolInEffectRow :: Eff '[] value -> Eff '[] value
__tidepoolInEffectRow = id
input = TidepoolHostInput.{{OPERAND_BINDER}}
{{CELL_DECLS}}
__tidepool_cell_check :: Eff '[] ()
__tidepool_cell_check = do { {{CELL_BODY}} ; pure () }
