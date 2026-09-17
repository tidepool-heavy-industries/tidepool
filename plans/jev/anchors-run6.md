ANCHOR_CLEAN (owned_file src/panels/list.rs; expect real_failure ~0.15, scope_creep ~0.02)
test panels::detail::tests::empty_notes_do_not_panic ... FAILED
test panels::help::tests::every_reachable_action_variant_appears_in_keymap ... FAILED
test panels::detail::tests::shows_no_item_when_empty ... FAILED
test panels::detail::tests::shows_title_and_notes_for_selected_item ... FAILED
test panels::help::tests::keymap_is_not_empty ... FAILED
test panels::list::tests::selection_moves_down_with_wraparound ... ok
test panels::list::tests::selection_moves_up_with_wraparound ... ok
test panels::list::tests::space_toggles_done ... ok
test panels::list::tests::empty_items_do_not_panic ... ok
test panels::status::tests::one_done_singular ... FAILED
test panels::status::tests::one_item_none_done ... FAILED
test panels::status::tests::multiple_items_some_done ... FAILED
test panels::status::tests::reflects_active_panel ... FAILED
test panels::status::tests::zero_items ... FAILED
test store::tests::round_trip ... FAILED
test panels::list::tests::render_contains_titles_and_done_marker ... ok
test store::tests::corrupt_file_returns_typed_error ... FAILED
test store::tests::missing_file_returns_io_error ... FAILED
thread 'panels::detail::tests::empty_notes_do_not_panic' (2430950) panicked at src/panels/detail.rs:42:9:
thread 'panels::help::tests::every_reachable_action_variant_appears_in_keymap' (2430953) panicked at src/panels/help.rs:33:5:
thread 'panels::detail::tests::shows_no_item_when_empty' (2430951) panicked at src/panels/detail.rs:42:9:
thread 'panels::detail::tests::shows_title_and_notes_for_selected_item' (2430952) panicked at src/panels/detail.rs:42:9:
thread 'panels::help::tests::keymap_is_not_empty' (2430954) panicked at src/panels/help.rs:33:5:
thread 'panels::status::tests::one_done_singular' (2430961) panicked at src/panels/status.rs:26:5:
thread 'panels::status::tests::one_item_none_done' (2430962) panicked at src/panels/status.rs:26:5:
thread 'panels::status::tests::multiple_items_some_done' (2430960) panicked at src/panels/status.rs:26:5:
thread 'panels::status::tests::reflects_active_panel' (2430963) panicked at src/panels/status.rs:26:5:
thread 'panels::status::tests::zero_items' (2430964) panicked at src/panels/status.rs:26:5:
thread 'store::tests::round_trip' (2430967) panicked at src/store.rs:51:5:
thread 'store::tests::corrupt_file_returns_typed_error' (2430965) panicked at src/store.rs:58:5:
thread 'store::tests::missing_file_returns_io_error' (2430966) panicked at src/store.rs:58:5:
test result: FAILED. 5 passed; 13 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

ANCHOR_FAILING (owned_file src/panels/status.rs; expect real_failure ~0.97)
test panels::help::tests::every_reachable_action_variant_appears_in_keymap ... ok
test panels::help::tests::keymap_is_not_empty ... ok
test panels::list::tests::selection_moves_down_with_wraparound ... ok
test panels::list::tests::selection_moves_up_with_wraparound ... ok
test panels::detail::tests::empty_notes_do_not_panic ... ok
test panels::detail::tests::shows_title_and_notes_for_selected_item ... ok
test panels::list::tests::space_toggles_done ... ok
test panels::detail::tests::shows_no_item_when_empty ... ok
test panels::status::tests::multiple_items_some_done ... ok
test panels::list::tests::empty_items_do_not_panic ... ok
test panels::status::tests::one_item_none_done ... ok
test panels::list::tests::render_contains_titles_and_done_marker ... ok
test panels::status::tests::reflects_active_panel ... ok
test panels::status::tests::one_done_singular ... ok
test panels::status::tests::zero_items ... FAILED
test store::tests::missing_file_returns_io_error ... ok
test store::tests::corrupt_file_returns_typed_error ... ok
test store::tests::round_trip ... ok
thread 'panels::status::tests::zero_items' (2431829) panicked at src/panels/status.rs:73:9:
test result: FAILED. 17 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
