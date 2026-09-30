module Tidepool.FamilyConsistency (validateCompilationFamilies) where

import GHC.Driver.Env (HscEnv, hscEPS, hsc_HPT)
import GHC.Tc.Types (TcGblEnv(..))
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt)
import GHC.Unit.Module.ModDetails (md_fam_insts)
import Tidepool.DeclarationJoin (JoinDecision(..), validateRetainedFamilyInstances)

-- Hidden original owners remain consistency inputs even when their lexical
-- instances are excluded from lookup. Every fresh frontend checks local
-- equations against that complete retained closure and the loaded packages.
validateCompilationFamilies :: HscEnv -> TcGblEnv -> IO ()
validateCompilationFamilies environment local = do
  packages <- hscEPS environment
  let retained = concatMap (md_fam_insts . hm_details) (eltsHpt (hsc_HPT environment))
        ++ tcg_fam_insts local
  case validateRetainedFamilyInstances (eps_fam_inst_env packages) retained of
    JoinAccepted -> pure ()
    JoinRejected reason diagnostic -> fail
      ("retained family consistency refused compilation (" ++ show reason ++ "): " ++ diagnostic)
