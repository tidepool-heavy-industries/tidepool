{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev)

type Situation = (Text, [Text], [Text], Text, Text, Text, Text, Text)
-- (sitId, ownedPaths, acceptanceChecklist, base, candidate, diffStat, hunks, testOutput)

situations :: [Situation]
situations =
  [ ( "S1_good_app_round1"
    , ["src/app.rs"]
    , [ "Item tags default empty and add_tag is idempotent without touching updated_at for duplicates."
      , "Tag filters participate in visible_indices and filter cycling returns from tag to all."
      , "Distinct tag counts are sorted and tests cover the shared contract."
      ]
    , "e8547e7145d0612127584a03e1d3f92e7ea52e7b"
    , "90a82467ec4f6bf15d414e0e9e4a6b86daec7c39"
    , "src/app.rs | 100 +++++++++++++++++++++++++++++++++++++------------\n 1 file changed, 96 insertions(+), 4 deletions(-)"
    , "+    pub tags: Vec<String>,\n+    pub fn add_tag(&mut self, tag: String) { if !self.tags.contains(&tag) { self.tags.push(tag); self.updated_at = now(); } }\n+pub enum Filter { All, Open, }\n+    pub tag_filter: Option<String>,\n+    pub fn cycle_filter(&mut self) { self.tag_filter = None; /* cycles All/Open, clears tag */ }\n+    pub fn tag_counts(&self) -> BTreeMap<String, usize> { /* sorted distinct counts */ }"
    , "running 36 tests\ntest app::tests::duplicate_tag_is_noop_without_touching_updated_at ... ok\ntest app::tests::visible_indices_respects_tag_filter ... ok\ntest app::tests::tags_are_distinct_counted_and_sorted ... ok\ntest app::tests::filter_cycle_returns_to_all_from_tag ... ok\ntest result: ok. 36 passed; 0 failed; 0 ignored"
    )
  , ( "S2_good_store"
    , ["src/store.rs"]
    , [ "Persistence round-trips tags for items that have them."
      , "Legacy items with no tags key load with an empty tag list."
      , "Existing store tests and errors are preserved."
      ]
    , "90a82467ec4f6bf15d414e0e9e4a6b86daec7c39"
    , "620a56d8b220b6aec2ad23df4d23b710e34bafc3"
    , "src/store.rs | 36 ++++++++++++++++++++++++++++++++++++\n 1 file changed, 36 insertions(+)"
    , "+#[test]\n+fn round_trip_preserves_tags() { let item = Item { tags: vec![\"a\".into(), \"b\".into()], ..Default::default() }; save(&item); let loaded = load(item.id).unwrap(); assert_eq!(loaded.tags, item.tags); }\n+#[test]\n+fn legacy_file_without_tags_loads_empty() { let json = r#\"{\\\"id\\\":1,\\\"title\\\":\\\"x\\\"}\"#; let item: Item = serde_json::from_str(json).unwrap(); assert!(item.tags.is_empty()); }"
    , "running 40 tests\ntest store::tests::round_trip_preserves_tags ... ok\ntest store::tests::legacy_file_without_tags_loads_empty ... ok\ntest result: ok. 40 passed; 0 failed; 0 ignored"
    )
  , ( "S3_missing_focus_defect"
    , ["src/app.rs"]
    , [ "Shared state needed by an interactive tags panel is added: enter/clear a tag filter while preserving the current all/open base filter."
      , "Selection state is safe for empty tags."
      , "Prior duplicate-timestamp and cycle_filter semantics keep passing."
      ]
    , "90a82467ec4f6bf15d414e0e9e4a6b86daec7c39"
    , "0dfb22503a010fe646af688284320b9c5cb5261f"
    , "src/app.rs | 80 +++++++++++++++++++++++++++++++++++++++++++++++++++-----\n 1 file changed, 76 insertions(+)"
    , "+    pub tag_base_filter: Filter,\n+    pub tag_selected: Option<String>,\n+    pub fn enter_tag_filter(&mut self, tag: String) { self.tag_base_filter = self.filter.clone(); self.tag_selected = Some(tag); }\n+    pub fn clear_tag_filter(&mut self) { self.filter = self.tag_base_filter.clone(); self.tag_selected = None; }\n+    // ActivePanel::Tags deferred: adding the enum variant would make existing consumers non-exhaustive until their owned files are updated."
    , "running 38 tests\ntest app::tests::tag_filter_preserves_base_on_enter ... ok\ntest app::tests::tag_filter_clear_restores_base ... ok\ntest result: ok. 38 passed; 0 failed; 0 ignored"
    )
  , ( "S10_missing_focus_defect_named_miss"
    , ["src/app.rs"]
    , [ "Shared state needed by an interactive tags panel is added: enter/clear a tag filter while preserving the current all/open base filter."
      , "Selection state is safe for empty tags."
      , "Prior duplicate-timestamp and cycle_filter semantics keep passing."
      , "A shared focus state (for example an ActivePanel::Tags variant or equivalent) exists so a Panel implementation can actually reach and select the new tag_selected state; state that no Panel can focus is not a complete contract."
      ]
    , "90a82467ec4f6bf15d414e0e9e4a6b86daec7c39"
    , "0dfb22503a010fe646af688284320b9c5cb5261f"
    , "src/app.rs | 80 +++++++++++++++++++++++++++++++++++++++++++++++++++-----\n 1 file changed, 76 insertions(+)"
    , "+    pub tag_base_filter: Filter,\n+    pub tag_selected: Option<String>,\n+    pub fn enter_tag_filter(&mut self, tag: String) { self.tag_base_filter = self.filter.clone(); self.tag_selected = Some(tag); }\n+    pub fn clear_tag_filter(&mut self) { self.filter = self.tag_base_filter.clone(); self.tag_selected = None; }\n+    // ActivePanel::Tags deferred: adding the enum variant would make existing consumers non-exhaustive until their owned files are updated."
    , "running 38 tests\ntest app::tests::tag_filter_preserves_base_on_enter ... ok\ntest app::tests::tag_filter_clear_restores_base ... ok\ntest result: ok. 38 passed; 0 failed; 0 ignored"
    )
  , ( "S4a_deliberate_red_no_exception"
    , ["src/app.rs"]
    , [ "ActivePanel::Tags exists as the minimal shared focus contract."
      , "check.sh passes with no non-exhaustive pattern errors."
      , "No other file outside src/app.rs changes."
      ]
    , "0dfb22503a010fe646af688284320b9c5cb5261f"
    , "f72688235d1b5483e37b14f7961a9e64eb85743e"
    , "src/app.rs | 14 +++++++++++++-\n 1 file changed, 10 insertions(+)"
    , "+pub enum ActivePanel { List, Detail, Tags }\n+#[test]\n+fn empty_tags_selection_is_safe() { let mut app = App::default(); app.enter_tag_filter(\"x\".into()); app.clear_tag_filter(); assert_eq!(app.tag_selected, None); }"
    , "./check.sh exited 101\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/panels/status.rs:29\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/panels/status.rs:49\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/main.rs:136\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/main.rs:151\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/main.rs:158"
    )
  , ( "S4b_deliberate_red_with_exception"
    , ["src/app.rs"]
    , [ "ActivePanel::Tags exists as the minimal shared focus contract."
      , "Non-exhaustive-match failures confined to src/main.rs and src/panels/status.rs are pre-authorized under the shared-contract-first exception in TASKS.md and do not block this candidate; the owning UI work will repair them before integration."
      , "No other file outside src/app.rs changes."
      ]
    , "0dfb22503a010fe646af688284320b9c5cb5261f"
    , "f72688235d1b5483e37b14f7961a9e64eb85743e"
    , "src/app.rs | 14 +++++++++++++-\n 1 file changed, 10 insertions(+)"
    , "+pub enum ActivePanel { List, Detail, Tags }\n+#[test]\n+fn empty_tags_selection_is_safe() { let mut app = App::default(); app.enter_tag_filter(\"x\".into()); app.clear_tag_filter(); assert_eq!(app.tag_selected, None); }"
    , "./check.sh exited 101\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/panels/status.rs:29\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/panels/status.rs:49\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/main.rs:136\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/main.rs:151\nerror[E0004]: non-exhaustive patterns: `ActivePanel::Tags` not covered\n  --> src/main.rs:158"
    )
  , ( "S5_readonly_check_failure"
    , ["src/app.rs"]
    , [ "check.sh output shows the fmt, clippy, and test stages all passing or the expected pre-authorized exhaustive-match errors only."
      , "No other file outside src/app.rs changes."
      ]
    , "0dfb22503a010fe646af688284320b9c5cb5261f"
    , "f72688235d1b5483e37b14f7961a9e64eb85743e"
    , "src/app.rs | 14 +++++++++++++-\n 1 file changed, 10 insertions(+)"
    , "+pub enum ActivePanel { List, Detail, Tags }"
    , "error: Read-only file system (os error 30) at path \"/tmp/tidepool-actor-workspace/targetsWm3Nz\"\ncargo check --all-targets: could not create target directory; no test or lint output was produced"
    )
  , ( "S6_gate_never_started"
    , []
    , [ "A worker candidate diff exists to review."
      , "check.sh output exists for that candidate."
      ]
    , "e8547e7145d0612127584a03e1d3f92e7ea52e7b"
    , ""
    , ""
    , ""
    , "Project.Gate failed to start: resident workbench execution failed: prepared engine: prepared compilation rejected: unsupported expression at node 18541 in binding ValueId(17401). Worker admission committed but no candidate diff has been produced yet; there is nothing to review."
    )
  , ( "S7_compressed_not_false_report"
    , ["src/app.rs"]
    , [ "Item tags default empty and add_tag is idempotent without touching updated_at for duplicates."
      , "Tag filters participate in visible_indices and filter cycling returns from tag to all."
      , "Distinct tag counts are sorted and tests cover the shared contract."
      , "worker_report: cargo fmt completed; cargo clippy --all-targets -- -D warnings completed; cargo test: 36 passed, 0 failed. (Note: this report names a single command run, but the actor's own session log shows two earlier failing ./check.sh attempts -- an E0204 Copy-trait error, then two clippy lints for dead_code and field_reassign_with_default -- fixed before this final green result.)"
      ]
    , "e8547e7145d0612127584a03e1d3f92e7ea52e7b"
    , "90a82467ec4f6bf15d414e0e9e4a6b86daec7c39"
    , "src/app.rs | 100 +++++++++++++++++++++++++++++++++++++------------\n 1 file changed, 96 insertions(+), 4 deletions(-)"
    , "+    pub tags: Vec<String>,\n+    pub fn add_tag(&mut self, tag: String) { if !self.tags.contains(&tag) { self.tags.push(tag); self.updated_at = now(); } }\n+    pub tag_filter: Option<String>,\n+    pub fn cycle_filter(&mut self) { self.tag_filter = None; }\n+    pub fn tag_counts(&self) -> BTreeMap<String, usize> { /* sorted distinct counts */ }"
    , "running 36 tests\ntest app::tests::duplicate_tag_is_noop_without_touching_updated_at ... ok\ntest app::tests::visible_indices_respects_tag_filter ... ok\ntest app::tests::tags_are_distinct_counted_and_sorted ... ok\ntest app::tests::filter_cycle_returns_to_all_from_tag ... ok\ntest result: ok. 36 passed; 0 failed; 0 ignored"
    )
  , ( "S8_store_tags_fmt_only_failure"
    , ["src/store.rs"]
    , [ "check.sh passes: fmt, clippy, and test stages all green."
      , "Named tests round_trip_preserves_tags and legacy_file_without_tags_loads_empty exist and pass."
      ]
    , "90a82467ec4f6bf15d414e0e9e4a6b86daec7c39"
    , "(uncommitted store.rs attempt, pre-fmt-fix)"
    , "src/store.rs | 36 ++++++++++++++++++++++++++++++++++++\n 1 file changed, 36 insertions(+)"
    , "+    fn round_trip_preserves_tags() { let path = std::env::temp_dir().join(format!(\"test_store_{}_{}.json\", std::process::id(), \"round_trip_preserves_tags_with_a_very_long_descriptive_name_that_overflows_the_line_length_limit\")); save_to(&path, &item); let loaded = load_from(&path).unwrap(); assert_eq!(loaded.tags, item.tags); }"
    , "cargo fmt --check\nDiff in src/store.rs at line 719-731 (line exceeds the configured max width)\ncheck.sh exited 1 at the fmt stage; clippy and test stages did not run"
    )
  , ( "S9_filter_enum_consumer_break"
    , ["src/app.rs"]
    , [ "Own only src/app.rs; change no other file."
      , "check.sh (the full workspace build and test) passes."
      ]
    , "e8547e7145d0612127584a03e1d3f92e7ea52e7b"
    , "(uncommitted app.rs attempt with Filter::Tag(String) enum variant, later abandoned)"
    , "src/app.rs | 60 +++++++++++++++++++++++++++++++++++++++----------\n 1 file changed, 56 insertions(+), 4 deletions(-)"
    , "+#[derive(Clone, Debug, PartialEq)]\n+pub enum Filter { All, Open, Tag(String) }\n+    pub fn add_tag(&mut self, tag: String) { if !self.tags.contains(&tag) { self.tags.push(tag); self.updated_at = now(); } }"
    , "cargo test app::tests: ok (narrow run, app module only, 12 passed)\ncargo test (full workspace): error[E0004]: non-exhaustive patterns: `app::Filter::Tag(_)` not covered\n  --> src/panels/list.rs:89\nerror[E0004]: non-exhaustive patterns: `app::Filter::Tag(_)` not covered\n  --> src/panels/status.rs:41\nerror[E0004]: non-exhaustive patterns: `app::Filter::Tag(_)` not covered\n  --> src/panels/status.rs:58\nerror[E0204]: the trait `Copy` cannot be implemented for this type because `String` is not `Copy`\n  --> src/app.rs:22"
    )
  ]

askGate :: Member Jev effs => Situation -> Eff effs (Text, Either Text (Text, Double, Double, Double, Double))
askGate (sid, owned, checklist, base, candidate, diffStat, hunks, testOutput) = do
  let allPresentText = "Every item of the checklist holds: " <> T.intercalate "; " checklist <> "."
      gate = J.choice "Which statement describes the candidate?"
        ( J.alt #all_present (String allPresentText) ()
          J..| J.alt #item_missing (String "At least one item does not hold: a changed file outside the owned file, a failing or missing owned test, a deleted or weakened test, a remaining todo!(), or an implementation that does not match the goal.") ()
          J..| J.alt #conflicting (String "The items are all present but contradict each other, for example the report claims a test passes that the test output shows failing.") ()
          J..| J.alt #insufficient_evidence (String "The state does not carry what the checklist needs to be decided: a file named in `diff_stat` has no hunk, or `test_output` names none of the required tests.") () )
      packet =
        #gate := gate
          :& #covered := J.noul "Does `hunks` contain a hunk for every file named in `diff_stat`?"
          :& Nil
  answer <- J.ask (J.state (object
    [ "owned_paths" .= owned
    , "acceptance_checklist" .= checklist
    , "base" .= base
    , "candidate" .= candidate
    , "diff_stat" .= diffStat
    , "hunks" .= hunks
    , "test_output" .= testOutput
    ])) packet
  case answer of
    Left e -> pure (sid, Left (T.pack (show e)))
    Right r -> do
      let a = J.answers r
      pure (sid, Right (a.gate.key, a.gate.confidence, a.gate.mass, a.gate.margin, a.covered.yes))

do
  results <- forM situations askGate
  pure (object
    [ "results" .=
        [ case res of
            Left e -> object ["id" .= sid, "error" .= e]
            Right (key, conf, mass, margin, coveredYes) -> object
              [ "id" .= sid
              , "key" .= key
              , "confidence" .= conf
              , "mass" .= mass
              , "margin" .= margin
              , "covered_yes" .= coveredYes
              ]
        | (sid, res) <- results
        ]
    ])
