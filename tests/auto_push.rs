use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "auto-push-{}-{}-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self(path);
        fixture.git(&["init", "-q", "--initial-branch=main", "local"]);
        fixture.git(&["init", "-q", "--bare", "remote.git"]);
        fixture.git(&["-C", "local", "config", "user.name", "Test"]);
        fixture.git(&[
            "-C",
            "local",
            "config",
            "user.email",
            "test@example.invalid",
        ]);
        fixture.git(&["-C", "local", "remote", "add", "origin", "../remote.git"]);
        fixture
    }
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    fn run(&self, total: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_git-auto-committer"))
            .args(["--repo-path"])
            .arg(self.0.join("local"))
            .args([
                "--total",
                total,
                "--batch-size",
                "700",
                "--batch-delay",
                "0",
            ])
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn pushes_at_each_thousand_independently_of_batches() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("local/sample.txt"), "snapshot content\n").unwrap();
    let output = fixture.run("2500");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.matches("Pushing after").count(), 2);
    assert!(stdout.contains("Pushing after 1000 commits"));
    assert!(stdout.contains("Pushing after 2000 commits"));
    assert_eq!(
        fixture.git(&["-C", "remote.git", "show", "refs/heads/main:sample.txt"]),
        "snapshot content"
    );
    let trees = fixture.git(&["-C", "local", "log", "--format=%T"]);
    assert!(trees.lines().all(|tree| Some(tree) == trees.lines().next()));
    assert_eq!(
        fixture.git(&["-C", "local", "rev-list", "--count", "HEAD"]),
        "2500"
    );
    assert_eq!(
        fixture.git(&["-C", "remote.git", "rev-list", "--count", "refs/heads/main"]),
        "2500"
    );
}

#[test]
fn pushes_remainder_before_a_thousand() {
    let fixture = Fixture::new();
    assert!(fixture.run("999").status.success());
    assert_eq!(
        fixture.git(&["-C", "remote.git", "rev-list", "--count", "refs/heads/main"]),
        "999"
    );
}

#[test]
fn push_failure_stops_commits_and_preserves_local_history() {
    let fixture = Fixture::new();
    fixture.git(&["-C", "local", "remote", "remove", "origin"]);
    let output = fixture.run("1001");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Auto-push failed"));
    assert_eq!(
        fixture.git(&["-C", "local", "rev-list", "--count", "HEAD"]),
        "1000"
    );
}

#[test]
fn configurable_interval_and_no_duplicate_final_push() {
    let fixture = Fixture::new();
    let output = Command::new(env!("CARGO_BIN_EXE_git-auto-committer"))
        .arg("-r")
        .arg(fixture.0.join("local"))
        .args(["-t", "6", "--push-every", "2", "-b", "0"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.matches("Pushing after").count(), 3);
    assert!(!stdout.contains("Pushing remaining"));
    assert_eq!(
        fixture.git(&["-C", "remote.git", "rev-list", "--count", "refs/heads/main"]),
        "6"
    );
}

#[cfg(unix)]
#[test]
fn retries_transient_push_failure() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let hook = fixture.0.join("local/.git/hooks/pre-push");
    fs::write(&hook, "#!/bin/sh\nif [ ! -f .git/retry-marker ]; then touch .git/retry-marker; echo 'Connection reset by peer' >&2; exit 1; fi\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_git-auto-committer"))
        .arg("-r")
        .arg(fixture.0.join("local"))
        .args(["-t", "1", "--push-retry-delay", "0"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("retry 1/3"));
    assert_eq!(
        fixture.git(&["-C", "remote.git", "rev-list", "--count", "refs/heads/main"]),
        "1"
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_flushes_pending_commits_during_batch_delay() {
    use std::{
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let fixture = Fixture::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_git-auto-committer"))
        .arg("-r")
        .arg(fixture.0.join("local"))
        .args(["-n", "1", "-b", "60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.0.join("local/.git/refs/heads/main").exists() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("no commit created");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("shutdown timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fixture.git(&["-C", "remote.git", "rev-parse", "refs/heads/main"]),
        fixture.git(&["-C", "local", "rev-parse", "HEAD"])
    );
}

#[cfg(unix)]
#[test]
fn transient_retries_are_bounded() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let hook = fixture.0.join("local/.git/hooks/pre-push");
    fs::write(&hook, "#!/bin/sh\necho attempt >> .git/push-attempts\necho 'Connection reset by peer' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_git-auto-committer"))
        .arg("-r")
        .arg(fixture.0.join("local"))
        .args(["-t", "1", "--push-retries", "2", "--push-retry-delay", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(fixture.0.join("local/.git/push-attempts"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    assert_eq!(
        fixture.git(&["-C", "local", "rev-list", "--count", "HEAD"]),
        "1"
    );
}
