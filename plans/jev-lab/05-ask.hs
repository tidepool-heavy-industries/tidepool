renderGroup :: Int -> ((Text, [Text]), [[Text]]) -> Text
renderGroup n ((headline, shared), ms) = T.unlines
  ( ("G" <> T.pack (show n) <> "| " <> headline)
  : ("  sites: " <> T.intercalate ", " (concatMap primaries ms))
  : [ "  shared: " <> s | s <- shared ]
  ++ [ "  one representative diagnostic, verbatim:" ]
  ++ map ("  | " <>) (head ms) )

askGroups owned cmd code gs =
  let keyed = [ ("G" <> T.pack (show n), n, g) | (n, g) <- zip [1 :: Int ..] gs ]
      gpool = J.pool #groups [ (k, String (renderGroup n g), g) | (k, n, g) <- keyed ]
      packet = #groups J.:= gpool
        J.:& #legible J.:= J.noul "Does `diagnostics` contain compiler diagnostics that name file locations?"
        J.:& #each J.:= J.eachIn gpool (\ref ->
             #edit_at_shared J.:= J.askAbout ref "Should the repair edit this group's shared location, rather than each of the listed sites?"
               J.:& #one_edit J.:= J.askAbout ref "Would a single edit resolve every listed site in this group?"
               J.:& #needs_shared_type J.:= J.askAbout ref "Does repairing this require changing a type or signature that code outside `owned_paths` depends on?"
               J.:& #suppressible J.:= J.askAbout ref "Is this a lint that should be allowed where it fires rather than fixed?"
               J.:& #compiler_fix_is_right J.:= J.askAbout ref "Is the fix the compiler suggests in its own help text the right repair here?"
               J.:& J.Nil)
        J.:& J.Nil
  in J.ask (J.state (object
       [ "command" .= cmd, "exit_status" .= code, "owned_paths" .= owned
       , "diagnostics" .= T.intercalate "\n" [ renderGroup n g | (n, g) <- zip [1 ..] gs ] ]))
     packet

ansA <- askGroups ["src/panels/"] "bash ./check.sh" (101 :: Int) (mechanicalGroups rawA)
either show (\r -> show (toJSON (J.answers r))) ansA
