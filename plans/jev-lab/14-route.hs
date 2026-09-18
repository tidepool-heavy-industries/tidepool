routeFindings owned (sym, keyed, verdict, found) =
  let per = case found of { Right r -> (J.answers r).each ; Left _ -> [] }
      placeholder = case verdict of
        Right r -> or [ g.help_text_placeholder.yes >= 0.9 | (_, g) <- (J.answers r).each ]
        Left _ -> False
      at k = maybe "?" id (lookup k [ (kk, l) | (kk, _, l) <- keyed ])
      why k = maybe "?" id (lookup k [ (kk, w) | (kk, w, _) <- keyed ])
      ownedBy l = any (`T.isPrefixOf` l) owned
      rows = [ (k, at k, why k, p) | (k, p) <- per ]
      mustEdit = [ (l, w) | (_, l, w, p) <- rows, p.must_change.yes >= 0.6 ]
  in object
     [ "symbol" .= sym
     , "must_edit" .= [ object ["at" .= l, "why" .= w] | (l, w) <- mustEdit ]
     , "outside_ownership" .= [ l | (l, _) <- mustEdit, not (ownedBy l) ]
     , "leave_alone" .= [ object ["at" .= l, "why" .= w] | (_, l, w, p) <- rows, p.must_change.yes < 0.6, p.declares.yes >= 0.6 ]
     , "already_covered_by_tests" .= [ l | (_, l, _, p) <- rows, p.is_test.yes >= 0.6 ]
     , "ignore_compiler_suggestion" .= placeholder ]

outA <- look "/home/inanna/dev/shoal-evals/tui-test-app" "f726882" ["src/panels/"] "bash ./check.sh" (101 :: Int) rawA
routeFindings ["src/panels/"] outA
