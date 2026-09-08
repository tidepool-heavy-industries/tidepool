#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use tidepool_node::MountNamespace;
use tidepool_worktree::testing::TestRepo;

struct Owner(Child);

impl Drop for Owner {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn host_git_observes_and_commits_the_actual_mounted_worktree() {
    let repository = TestRepo::init().unwrap();
    let git = repository.git();
    std::fs::write(repository.path().join("file"), "inherited\n").unwrap();
    git.try_run(repository.path(), &["add", "file"]).unwrap();
    git.try_run(repository.path(), &["commit", "-qm", "seed"])
        .unwrap();
    let original_head = git
        .try_run(repository.path(), &["rev-parse", "HEAD"])
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    for name in ["base", "upper", "work"] {
        std::fs::create_dir(storage.path().join(name)).unwrap();
    }
    let base = storage.path().join("base");
    let upper = storage.path().join("upper");
    let work = storage.path().join("work");
    let view = storage.path().join("view");
    std::fs::copy(repository.path().join("file"), base.join("file")).unwrap();
    git.try_run(
        repository.path(),
        &[
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            "child",
            view.to_str().unwrap(),
            "HEAD",
        ],
    )
    .unwrap();
    std::fs::copy(view.join(".git"), upper.join(".git")).unwrap();
    let mut owner = Owner(
        Command::new("bwrap")
            .args([
                "--unshare-user",
                "--bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--overlay-src",
            ])
            .arg(&base)
            .arg("--overlay")
            .arg(&upper)
            .arg(&work)
            .arg(&view)
            .args([
                "--die-with-parent",
                "--",
                "/bin/sh",
                "-c",
                "printf '%s\\n' \"$$\"; read line || exit 0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(owner.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let namespace = MountNamespace::capture(line.trim().parse().unwrap()).unwrap();
    let mut probe = namespace
        .host_command(&view, std::ffi::OsStr::new("/bin/sh"))
        .unwrap();
    let output = probe
        .args(["-c", "cat /proc/self/status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status = String::from_utf8(output.stdout).unwrap();
    for field in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(field))
            .unwrap();
        assert_eq!(value.trim(), "0000000000000000", "{field}");
    }
    assert!(status.lines().any(|line| line == "NoNewPrivs:\t1"));
    let mounted_git = git.with_mount_namespace(namespace.clone());
    mounted_git.try_run(&view, &["read-tree", "HEAD"]).unwrap();
    assert_eq!(
        mounted_git
            .try_run(&view, &["status", "--porcelain"])
            .unwrap()
            .trimmed(),
        ""
    );
    assert!(
        !view.join("file").exists(),
        "the host's underlying directory is deliberately not the mounted view"
    );
    mounted_git
        .try_run(&view, &["mv", "file", "renamed"])
        .unwrap();
    mounted_git
        .try_run(&view, &["commit", "-qm", "child change"])
        .unwrap();
    assert_eq!(
        git.try_run(repository.path(), &["show", "child:renamed"])
            .unwrap()
            .trimmed(),
        "inherited"
    );
    assert_eq!(
        git.try_run(repository.path(), &["rev-parse", "HEAD"])
            .unwrap(),
        original_head
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file")).unwrap(),
        "inherited\n"
    );
    let mut prepared = namespace
        .host_command(&view, std::ffi::OsStr::new("/bin/sh"))
        .unwrap();
    prepared.args(["-c", "exit 0"]);
    drop(owner.0.stdin.take());
    assert!(owner.0.wait().unwrap().success());
    assert!(
        prepared.output().is_err(),
        "owner liveness must be checked at spawn as well as preparation"
    );
    assert!(
        mounted_git.run(&view, &["status", "--porcelain"]).is_err(),
        "a dead owner must never silently select the underlying directory"
    );
    assert_eq!(
        namespace.require_live_owner().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}
