askNarrow owned cmd code gs =
  let keyed = [ ("G" <> T.pack (show n), n, g) | (n, g) <- zip [1 :: Int ..] gs ]
      gpool = J.pool #groups [ (k, String (renderGroup n g), g) | (k, n, g) <- keyed ]
      packet = #groups J.:= gpool
        J.:& #legible J.:= J.noul "Does `diagnostics` contain compiler diagnostics that name file locations?"
        J.:& #each J.:= J.eachIn gpool (\ref ->
             #shared_is_correct J.:= J.askAbout ref "Is the code at this group's shared location already correct, so that the repair must change the listed sites instead?"
               J.:& #each_site_separate J.:= J.askAbout ref "Does each listed site need its own separate edit, because the sites are in different functions or files?"
               J.:& #outside_owned J.:= J.askAbout ref "Is at least one listed site in a file that does not start with any prefix in `owned_paths`?"
               J.:& #help_text_placeholder J.:= J.askAbout ref "Does the compiler's own suggested fix in the verbatim diagnostic insert a placeholder such as `todo!()` or `unimplemented!()` rather than working code?"
               J.:& J.Nil)
        J.:& J.Nil
  in J.ask (J.state (object
       [ "command" .= cmd, "exit_status" .= code, "owned_paths" .= owned
       , "diagnostics" .= T.intercalate "\n" [ renderGroup n g | (n, g) <- zip [1 ..] gs ] ]))
     packet

narrowA <- askNarrow ["src/panels/"] "bash ./check.sh" (101 :: Int) (mechanicalGroups rawA)
narrowB <- askNarrow ["src/panels/"] "bash ./check.sh" (101 :: Int) (mechanicalGroups rawB)
object [ "A" .= either (\e -> String (T.pack (show e))) (toJSON . J.answers) narrowA
       , "B" .= either (\e -> String (T.pack (show e))) (toJSON . J.answers) narrowB ]
