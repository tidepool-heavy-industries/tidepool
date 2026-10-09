module Tidepool.FamilyConsistency
  ( FamilyConsistencyRejection(..), renderFamilyConsistencyRejection
  , validateCompilationFamilies, validateEnvironmentFamilies
  ) where

import Control.Exception (Exception, throwIO)
import GHC.Driver.Env (HscEnv, hscEPS, hsc_HPT)
import GHC.Tc.Types (TcGblEnv(..))
import GHC.Core.FamInstEnv (FamInst)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt)
import GHC.Unit.Module.ModDetails (md_fam_insts)
import Tidepool.DeclarationJoin (JoinDecision(..), JoinRejection, validateRetainedFamilyInstances)

-- A retained equation conflict is a source refusal, independent of which
-- frontend or worker boundary performs the consistency check.
data FamilyConsistencyRejection = FamilyConsistencyRejection JoinRejection String
  deriving Show
instance Exception FamilyConsistencyRejection

renderFamilyConsistencyRejection :: FamilyConsistencyRejection -> String
renderFamilyConsistencyRejection (FamilyConsistencyRejection reason diagnostic) =
  "retained family consistency refused compilation (" ++ show reason ++ "): " ++ diagnostic

-- Hidden original owners remain consistency inputs even when their lexical
-- instances are excluded from lookup. Every fresh frontend checks local
-- equations against that complete retained closure and the loaded packages.
validateCompilationFamilies :: HscEnv -> TcGblEnv -> IO ()
validateCompilationFamilies environment local =
  validateFamilies environment (tcg_fam_insts local)

-- Successful source-load interfaces and exact hidden originals coexist only
-- after hydration. Check their equations before reusing any loaded metadata.
validateEnvironmentFamilies :: HscEnv -> IO ()
validateEnvironmentFamilies environment = validateFamilies environment []

validateFamilies :: HscEnv -> [FamInst] -> IO ()
validateFamilies environment local = do
  packages <- hscEPS environment
  let retained = concatMap (md_fam_insts . hm_details) (eltsHpt (hsc_HPT environment))
        ++ local
  case validateRetainedFamilyInstances (eps_fam_inst_env packages) retained of
    JoinAccepted -> pure ()
    JoinRejected reason diagnostic -> throwIO (FamilyConsistencyRejection reason diagnostic)
