use tidepool_worktree::testing::{fingerprint, TestRepo};

#[test]
fn scripted_writer_drives_a_real_repository() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    let a = w.commit_file("a.txt", "one", "first").expect("commit");
    let b = w.commit_file("b.txt", "two", "second").expect("commit");
    assert_ne!(a, b);
    assert_eq!(w.current_branch().unwrap().unwrap().as_str(), "main");
    let amended = w.amend("second, reworded").expect("amend");
    assert_ne!(amended, b);
    let rewound = w.reset_hard(a.as_str()).expect("reset");
    assert_eq!(rewound, a);
    w.checkout_new_branch("side").expect("branch");
    assert_eq!(w.current_branch().unwrap().unwrap().as_str(), "side");
    let fp = fingerprint::working_tree(repo.path());
    assert!(
        fp.contains_key("a.txt"),
        "fingerprint sees the working tree: {fp:?}"
    );
    assert!(
        !fp.keys().any(|k| k.starts_with(".git")),
        "no .git in fingerprint"
    );
}
