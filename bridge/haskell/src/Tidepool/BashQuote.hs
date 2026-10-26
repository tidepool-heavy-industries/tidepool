{-# LANGUAGE TemplateHaskell #-}

-- | The held-world literal Bash expansion is authorized by the selected
-- compiler inputs, independently of the spelling used to import the quoter.
module Tidepool.BashQuote
  ( BashQuoteAuthority, resolveBashQuoteAuthority, bashQuoteName
  , bashQuoteNames, bashQuoteExpression
  ) where

import Control.Exception (evaluate)
import Control.Monad (forM, unless)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC.Driver.Env (HscEnv, hsc_HUG, hscEPS, lookupIfaceByModule, lookupType)
import GHC.Hs
import GHC.Data.FastString (FastString)
import GHC.Types.Avail (availNames)
import GHC.Types.Id (idName)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (RdrName(Exact))
import GHC.Types.Name.Set (NameSet, mkNameSet)
import GHC.Types.SourceText (SourceText(NoSourceText))
import GHC.Types.TyThing (TyThing(AnId))
import GHC.Unit.External (ExternalPackageState(eps_PIT))
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_exports, mi_src_hash, mi_hsc_src, mi_iface_hash, mi_final_exts)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GHC.Utils.Fingerprint (fingerprintByteString)
import Tidepool.ExactHydration (CapturedInterfaceInputs, selectVerifiedExactInterfaceProducts, verifiedExactInterface)
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))
import Tidepool.ExactScope
  ( CanonicalInterfaceAdmission(..), canonicalInterfaceInput, canonicalOrigin, isSourceOriginal, ShippedHomeModuleOrigin(..), CanonicalAdmissionKind(..)
  , resolveShippedHomeModuleWithEvidence )

-- These files are declared native compiler inputs. Changing the implementation
-- changes the compiler's audit, rather than accepting a same-named replacement.
shippedBashSource :: BS.ByteString
shippedBashSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib/Tidepool/QQ/Bash.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

shippedCommandSource :: BS.ByteString
shippedCommandSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib/Tidepool/Command/Types.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

shippedTextSource :: BS.ByteString
shippedTextSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib/Tidepool/Data/Text.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

-- Retain the actual selected interface allocations along with their exported
-- Names. The constructor is private; an occurrence or reconstructed Name cannot
-- issue this expansion authority.
data BashQuoteAuthority = BashQuoteAuthority Name Name Name [(ModIface,Maybe CanonicalInterfaceAdmission)]

bashQuoteName :: BashQuoteAuthority -> Name
bashQuoteName (BashQuoteAuthority quoter _ _ _) = quoter

bashQuoteNames :: BashQuoteAuthority -> NameSet
bashQuoteNames (BashQuoteAuthority quoter command pack _) = mkNameSet [quoter,command,pack]

resolveBashQuoteAuthority :: HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission -> CapturedInterfaceInputs -> IO BashQuoteAuthority
resolveBashQuoteAuthority env admitted captured = do
  selected <- forM
    [("Tidepool.QQ.Bash", "bash", shippedBashSource)
    ,("Tidepool.Command.Types", "bashCommand", shippedCommandSource)
    ,("Tidepool.Data.Text", "pack", shippedTextSource)] $ \(owner,occurrence,source) -> do
      resolved <- resolveShippedHomeModuleWithEvidence env admitted owner source
      actual <- case resolved of
        Right (actual,ShippedPhysicalSource) -> pure actual
        Right (actual,ShippedCanonicalSource CanonicalSourceOrigin _) -> pure actual
        _ -> fail ("literal Bash source authority refused: " ++ owner)
      external <- hscEPS env
      interface <- maybe (fail "literal Bash lacks its loaded selected interface") pure
        (lookupIfaceByModule (hsc_HUG env) (eps_PIT external) actual)
      unless (mi_module interface == actual && mi_src_hash interface == fingerprintByteString source)
        (fail "literal Bash loaded another source or interface owner")
      let selected = Map.lookup (unitString (moduleUnit actual),moduleNameString (moduleName actual)) admitted
      case selected of
        Just (ModuleInterfaceAdmission proof) -> do
          unless (isSourceOriginal (canonicalOrigin proof))
            (fail "literal Bash requires original source authority")
          let (artifact,_,_) = canonicalInterfaceInput proof
          verified <- either fail pure (selectVerifiedExactInterfaceProducts captured [artifact])
          expected <- case verified of
            [original] -> pure (snd (verifiedExactInterface original))
            _ -> fail "literal Bash lost its selected interface capture"
          unless (mi_module expected == mi_module interface
              && mi_hsc_src expected == mi_hsc_src interface
              && mi_iface_hash (mi_final_exts expected) == mi_iface_hash (mi_final_exts interface)
              && Set.fromList (concatMap availNames (mi_exports expected))
                  == Set.fromList (concatMap availNames (mi_exports interface)))
            (fail "literal Bash interface differs from its selected captured owner")
        Just (LocalInterfaceAdmission _) -> fail "literal Bash cannot borrow native declaration authority"
        Nothing -> pure ()
      name <- case [name | name <- concatMap availNames (mi_exports interface)
        , nameModule_maybe name == Just actual, occNameString (nameOccName name) == occurrence] of
        [name] -> pure name
        _ -> fail ("literal Bash lacks its unique defining export: " ++ owner ++ "." ++ occurrence)
      thing <- lookupType env name
      case thing of
        Just (AnId identifier) | idName identifier == name -> pure ()
        _ -> fail "literal Bash export lacks its hydrated identifier"
      retained <- evaluate interface
      pure (name,(retained,selected))
  case selected of
    [(quoter,q),(command,c),(pack,t)] -> pure (BashQuoteAuthority quoter command pack [q,c,t])
    _ -> fail "literal Bash authority lost a required owner"

-- HsString has the same String payload as the shipped quoteExp's StringL.
-- It is not an overloaded literal and does not interpret shell interpolation.
bashQuoteExpression :: BashQuoteAuthority -> FastString -> HsExpr GhcPs
bashQuoteExpression (BashQuoteAuthority _ command pack _) source =
  HsApp noExtField (nlHsVar (Exact command))
    (noLocA (HsApp noExtField (nlHsVar (Exact pack))
      (noLocA (HsLit noExtField (HsString NoSourceText source)))))
