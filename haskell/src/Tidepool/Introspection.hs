module Tidepool.Introspection
  ( InspectionResult(..)
  , InfoEntry(..)
  , runInspection
  , encodeInspectionResult
  ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (forM)
import Control.Monad.IO.Class (liftIO)
import qualified Data.ByteString as BS
import qualified Data.Text as T
import GHC
import GHC.Iface.Type (ShowForAllFlag(..), ShowHowMuch(..), ShowSub(..))
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, globalRdrEnvElts, greName)
import GHC.Types.TyThing.Ppr (pprTyThingInContext)
import GHC.Utils.Outputable (defaultSDocContext, renderWithContext)
import Tidepool.ExtractRequest (InspectionRequest(..))
import Tidepool.ExtractUtil (getLibdir)

data InfoEntry = InfoEntry
  { infoName :: String
  , infoModule :: Maybe String
  , infoKind :: String
  , infoDisplay :: String
  }
  deriving (Eq, Show)

data InspectionResult
  = InspectionType String String
  | InspectionInfo String [InfoEntry]
  | InspectionAmbiguous String [InfoEntry]
  | InspectionNotFound String
  deriving (Eq, Show)

runInspection :: HscEnv -> GlobalRdrEnv -> Maybe String -> InspectionRequest -> IO InspectionResult
runInspection hscEnv rdrEnv capturedType request = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    setSession hscEnv
    case request of
      InspectTypeOf expression -> case capturedType of
        Just display -> pure (InspectionType expression display)
        Nothing -> liftIO (ioError (userError "inspection module did not expose __user type"))
      InspectNameInfo query -> do
        let names = [ greName gre
                    | gre <- globalRdrEnvElts rdrEnv
                    , matchesQuery query (greName gre) ]
        entries <- fmap concat $ forM names $ \name -> do
          found <- getInfo False name
          pure $ case found of
            Nothing -> []
            Just (thing, _fixity, _instances, _families, _extra) ->
              let display = renderWithContext defaultSDocContext
                    (pprTyThingInContext showEverything thing)
                  definingModule = moduleNameString . moduleName <$> nameModule_maybe name
              in [InfoEntry
                    { infoName = occNameString (nameOccName name)
                    , infoModule = definingModule
                    , infoKind = thingKind thing
                    , infoDisplay = display
                    }]
        let preferred = if any ((== "type") . infoKind) entries
              then filter ((/= "constructor") . infoKind) entries
              else entries
        pure $ case preferred of
          [] -> InspectionNotFound query
          [_] -> InspectionInfo query preferred
          _ -> InspectionAmbiguous query preferred

matchesQuery :: String -> Name -> Bool
matchesQuery query name =
  occNameString (nameOccName name) == occurrence
    && maybe True (\wanted -> definingModule == Just wanted) qualifier
  where
    (qualifier, occurrence) = case break (== '.') (reverse query) of
      (reversedOccurrence, []) -> (Nothing, reverse reversedOccurrence)
      (reversedOccurrence, _ : reversedQualifier) ->
        (Just (reverse reversedQualifier), reverse reversedOccurrence)
    definingModule = moduleNameString . moduleName <$> nameModule_maybe name

showEverything :: ShowSub
showEverything = ShowSub ShowIface ShowForAllWhen

thingKind :: TyThing -> String
thingKind thing = case thing of
  AnId _ -> "value"
  AConLike _ -> "constructor"
  ATyCon _ -> "type"
  ACoAxiom _ -> "coercion"

-- | Private V1 inspection receipt. The outer list is @["TPINSP001", body]@;
-- each body is a closed tagged product decoded strictly by Rust.
encodeInspectionResult :: InspectionResult -> BS.ByteString
encodeInspectionResult result = toStrictByteString $
  encodeListLen 2 <> encodeString "TPINSP001" <> encodeResult result
  where
    encodeResult inspection = case inspection of
      InspectionType expression display ->
        encodeListLen 3 <> encodeString "Type" <> text expression <> text display
      InspectionInfo query entries ->
        encodeListLen 3 <> encodeString "Info" <> text query
          <> encodeListLen (fromIntegral (length entries)) <> foldMap encodeEntry entries
      InspectionAmbiguous query entries ->
        encodeListLen 3 <> encodeString "Ambiguous" <> text query
          <> encodeListLen (fromIntegral (length entries)) <> foldMap encodeEntry entries
      InspectionNotFound query ->
        encodeListLen 2 <> encodeString "NotFound" <> text query
    encodeEntry entry =
      encodeListLen 4 <> text (infoName entry) <> maybe encodeNull text (infoModule entry)
        <> text (infoKind entry) <> text (infoDisplay entry)
    text = encodeString . T.pack
