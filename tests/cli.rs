use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Output, Stdio},
};
use tempfile::TempDir;

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aimark"))
        .current_dir(root)
        .arg("--root")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .unwrap()
}
fn ok(root: &Path, args: &[&str]) -> String {
    let o = cli(root, args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}
fn git(root: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}
fn blame(root: &Path, file: &str) -> Value {
    serde_json::from_str(&ok(root, &["blame", file, "--json"])).unwrap()
}
fn write(root: &Path, file: &str, text: &str) {
    fs::write(root.join(file), text).unwrap();
}
fn spawn_hook(root: &Path, event: &str, file: Option<&Path>) -> Child {
    let mut p = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .current_dir(root)
        .arg("--root")
        .arg(root)
        .args(["hook", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = json!({"hook_event_name":event,"tool_name":"Write","tool_input":{"file_path":file},"session_id":"test-session"});
    p.stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    p
}

fn hook(root: &Path, event: &str, file: Option<&Path>) -> Output {
    spawn_hook(root, event, file).wait_with_output().unwrap()
}

fn ok_hook(root: &Path, event: &str, file: &Path) {
    let output = hook(root, event, Some(file));
    assert!(
        output.status.success(),
        "{event}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn mixed_attribution_and_pending_reports_are_read_only() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.rs", "fn main() {\n    human();\n}\n");
    ok(r, &["checkpoint"]);
    write(r, "a.rs", "fn main() {\n    human();\n    robot();\n}\n");
    ok(
        r,
        &[
            "record",
            "--harness",
            "test",
            "--model",
            "model",
            "--files",
            "a.rs",
        ],
    );
    let b = blame(r, "a.rs");
    assert_eq!(
        b.as_array()
            .unwrap()
            .iter()
            .map(|l| l["ai"].as_bool().unwrap())
            .collect::<Vec<_>>(),
        [false, false, true, false]
    );
    assert_eq!(b[2]["author"], "test/model");
    let before = git(r, &["--git-dir=.aimark/repo", "rev-parse", "HEAD"]);
    write(r, "a.rs", "fn main() {\n    changed();\n    robot();\n}\n");
    let b = blame(r, "a.rs");
    assert_eq!(b[1]["pending"], true);
    assert_eq!(b[2]["ai"], true);
    let s: Value = serde_json::from_str(&ok(r, &["stat", "--json"])).unwrap();
    assert_eq!(s["ai"], 1);
    assert_eq!(s["total"], 4);
    assert_eq!(s["pct"], 25.0);
    assert_eq!(
        git(r, &["--git-dir=.aimark/repo", "rev-parse", "HEAD"]),
        before
    );
}

#[test]
fn canonical_project_roots_work_from_an_unrelated_directory() {
    let storage = TempDir::new().unwrap();
    let caller = TempDir::new().unwrap();
    let project = storage.path().join("project with spaces");
    fs::create_dir(&project).unwrap();
    // On Windows canonicalize produces the verbatim path prefix rejected by Git path arguments.
    let root = project.canonicalize().unwrap();
    let invoke = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_aimark"))
            .current_dir(caller.path())
            .arg("--root")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    write(&root, "a.txt", "human\n");
    invoke(&["checkpoint"]);
    write(&root, "a.txt", "robot\n");
    invoke(&[
        "record",
        "--harness",
        "test",
        "--files",
        root.join("a.txt").to_str().unwrap(),
    ]);
    let lines: Value = serde_json::from_str(&invoke(&["blame", "a.txt", "--json"])).unwrap();
    assert_eq!(lines[0]["ai"], true);
    assert_eq!(lines[0]["text"], "robot");
    assert!(root.join(".aimark/repo/HEAD").exists());
    assert!(!caller.path().join(".aimark").exists());
}
#[test]
fn empty_projects_and_untracked_files() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    ok(r, &["init"]);
    let s: Value = serde_json::from_str(&ok(r, &["stat", "--json"])).unwrap();
    assert_eq!(s["total"], 0);
    write(r, "new.txt", "hello\n");
    let b = blame(r, "new.txt");
    assert_eq!(b[0]["ai"], false);
    assert_eq!(b[0]["pending"], true);
}
#[test]
fn root_discovery_from_subdirectories() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    fs::create_dir(r.join("src")).unwrap();
    write(r, "src/a.rs", "human\n");
    ok(r, &["checkpoint"]);
    let output = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .current_dir(r.join("src"))
        .args(["blame", "a.rs", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(lines[0]["text"], "human");
    assert!(!r.join("src/.aimark").exists());
}
#[cfg(unix)]
#[test]
fn literal_selected_paths_and_unusual_filenames() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    for f in ["a,b.txt", "a[1].txt", "other.txt", "line\nbreak.txt"] {
        write(r, f, "human\n");
    }
    ok(r, &["checkpoint"]);
    for f in ["a,b.txt", "a[1].txt", "other.txt", "line\nbreak.txt"] {
        write(r, f, "robot\n");
    }
    ok(
        r,
        &[
            "record",
            "--harness",
            "test",
            "--files",
            "a,b.txt",
            "--files",
            "a[1].txt",
            "--files",
            "line\nbreak.txt",
        ],
    );
    assert_eq!(blame(r, "a,b.txt")[0]["ai"], true);
    assert_eq!(blame(r, "a[1].txt")[0]["ai"], true);
    assert_eq!(blame(r, "other.txt")[0]["pending"], true);
    let s: Value = serde_json::from_str(&ok(r, &["stat", "--json"])).unwrap();
    assert_eq!(s["total"], 4);
    assert_eq!(s["ai"], 3);
}
#[test]
fn invalid_paths_fail_before_ledger_mutation() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human\n");
    ok(r, &["checkpoint"]);
    let before = git(r, &["--git-dir=.aimark/repo", "rev-parse", "HEAD"]);
    write(r, "a.txt", "robot\n");
    for p in [
        "../outside.txt",
        ".aimark/repo/HEAD",
        ".git/config",
        ":(glob)**",
    ] {
        assert!(
            !cli(r, &["record", "--harness", "test", "--files", p])
                .status
                .success(),
            "accepted {p}"
        );
    }
    assert_eq!(
        git(r, &["--git-dir=.aimark/repo", "rev-parse", "HEAD"]),
        before
    );
}
#[test]
fn ignores_and_binary_files() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, ".gitignore", "build/\nsecret.txt\n");
    fs::create_dir(r.join("build")).unwrap();
    write(r, "build/a.txt", "artifact\n");
    write(r, "secret.txt", "secret\n");
    write(r, "a.txt", "human\n");
    fs::write(r.join("binary"), [0, 1, 2]).unwrap();
    ok(r, &["checkpoint"]);
    let s: Value = serde_json::from_str(&ok(r, &["stat", "--json"])).unwrap();
    assert_eq!(s["files"].as_array().unwrap().len(), 2);
    assert!(!cli(r, &["blame", "secret.txt", "--json"]).status.success());
}
#[test]
fn scoped_hooks_preserve_overlapping_edits() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human a\n");
    write(r, "b.txt", "human b\n");
    ok(r, &["checkpoint"]);
    ok_hook(r, "PreToolUse", &r.join("a.txt"));
    write(r, "a.txt", "robot a\n");
    ok_hook(r, "PreToolUse", &r.join("b.txt"));
    write(r, "b.txt", "robot b\n");
    ok_hook(r, "PostToolUse", &r.join("a.txt"));
    ok_hook(r, "PostToolUse", &r.join("b.txt"));
    assert_eq!(blame(r, "a.txt")[0]["ai"], true);
    assert_eq!(blame(r, "b.txt")[0]["ai"], true);
}
#[cfg(unix)]
#[test]
fn hook_paths_accept_symlinked_project_ancestors() {
    use std::os::unix::fs::symlink;
    let storage = TempDir::new().unwrap();
    let aliases = TempDir::new().unwrap();
    let root = storage.path().join("project");
    fs::create_dir(&root).unwrap();
    symlink(storage.path(), aliases.path().join("storage")).unwrap();
    let alias = aliases.path().join("storage/project");
    write(&root, "existing.txt", "human\n");
    ok(&root, &["checkpoint"]);

    ok_hook(&root, "PreToolUse", &alias.join("existing.txt"));
    write(&root, "existing.txt", "robot\n");
    ok_hook(&root, "PostToolUse", &alias.join("existing.txt"));
    assert_eq!(blame(&root, "existing.txt")[0]["ai"], true);
    assert_eq!(
        blame(&root, alias.join("existing.txt").to_str().unwrap())[0]["ai"],
        true
    );

    ok_hook(&root, "PreToolUse", &alias.join("new.txt"));
    write(&root, "new.txt", "new robot line\n");
    ok_hook(&root, "PostToolUse", &alias.join("new.txt"));
    assert_eq!(blame(&root, "new.txt")[0]["ai"], true);

    ok_hook(&root, "PreToolUse", &alias.join("existing.txt"));
    fs::remove_file(root.join("existing.txt")).unwrap();
    ok_hook(&root, "PostToolUse", &alias.join("existing.txt"));
    let stats: Value = serde_json::from_str(&ok(&root, &["stat", "--json"])).unwrap();
    assert_eq!(stats["ai"], 1);
    assert_eq!(stats["total"], 1);
}

#[cfg(unix)]
#[test]
fn aliased_project_ancestors_do_not_allow_symlinks_inside_project() {
    use std::os::unix::fs::symlink;
    let storage = TempDir::new().unwrap();
    let aliases = TempDir::new().unwrap();
    let root = storage.path().join("project");
    fs::create_dir(&root).unwrap();
    symlink(storage.path(), aliases.path().join("storage")).unwrap();
    symlink(&root, root.join("self")).unwrap();
    write(&root, "a.txt", "human\n");
    ok(&root, &["checkpoint"]);
    let output = hook(
        &root,
        "PreToolUse",
        Some(&aliases.path().join("storage/project/self/a.txt")),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("symlink paths cannot be selected"));
}

#[test]
fn malformed_hooks_never_attribute_whole_project() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human\n");
    ok(r, &["checkpoint"]);
    write(r, "a.txt", "pending\n");
    assert!(!hook(r, "PostToolUse", None).status.success());
    assert_eq!(blame(r, "a.txt")[0]["ai"], false);
    assert_eq!(blame(r, "a.txt")[0]["pending"], true);
}
#[test]
fn preserves_real_git_index() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    git(r, &["init", "-q"]);
    write(r, "a.txt", "staged\n");
    git(r, &["add", "a.txt"]);
    let index = fs::read(r.join(".git/index")).unwrap();
    write(r, "a.txt", "unstaged\n");
    ok(r, &["checkpoint"]);
    assert_eq!(fs::read(r.join(".git/index")).unwrap(), index);
    assert!(!git(r, &["status", "--porcelain"]).contains(".aimark"));
    assert!(git(r, &["diff", "--cached", "--", "a.txt"]).contains("+staged"));
}
#[cfg(unix)]
#[test]
fn wrapper_preserves_failed_exit_and_caller_directory() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    fs::create_dir(r.join("src")).unwrap();
    write(r, "src/a.txt", "human\n");
    ok(r, &["checkpoint"]);
    let o = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .current_dir(r.join("src"))
        .args([
            "run",
            "--harness",
            "shell",
            "--",
            "sh",
            "-c",
            "printf 'robot\n' > a.txt; exit 7",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(7));
    assert_eq!(blame(r, "src/a.txt")[0]["ai"], true);
    assert!(!r.join("a.txt").exists());
}
#[cfg(unix)]
#[test]
fn escaping_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let d = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let r = d.path();
    write(outside.path(), "secret", "private\n");
    symlink(outside.path().join("secret"), r.join("link")).unwrap();
    ok(r, &["checkpoint"]);
    assert!(!cli(r, &["blame", "link", "--json"]).status.success());
    assert!(
        !cli(r, &["record", "--harness", "test", "--files", "link"])
            .status
            .success()
    );
}

#[test]
fn whitespace_formatting_rename_and_deletion_preserve_history() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.rs", "fn main() {\n    human();\n}\n");
    ok(r, &["checkpoint"]);
    write(r, "a.rs", "fn main() {\n    human();\n    robot();\n}\n");
    ok(r, &["record", "--harness", "test"]);
    write(r, "a.rs", "fn main() {\n  human();\n  robot();\n}\n");
    ok(r, &["checkpoint"]);
    assert_eq!(blame(r, "a.rs")[2]["ai"], true);
    fs::rename(r.join("a.rs"), r.join("renamed.rs")).unwrap();
    ok(r, &["checkpoint"]);
    assert_eq!(blame(r, "renamed.rs")[2]["ai"], true);
    fs::remove_file(r.join("renamed.rs")).unwrap();
    ok(r, &["record", "--harness", "test", "--files", "renamed.rs"]);
    let s: Value = serde_json::from_str(&ok(r, &["stat", "--json"])).unwrap();
    assert_eq!(s["total"], 0);
}

#[test]
fn concurrent_first_use_is_serialized() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human\n");
    let mut children = Vec::new();
    for _ in 0..6 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_aimark"))
                .arg("--root")
                .arg(r)
                .arg("checkpoint")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    assert_eq!(blame(r, "a.txt")[0]["pending"], false);
}

#[test]
fn inherited_git_environment_cannot_redirect_ledger_operations() {
    let d = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human\n");
    let output = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .arg("--root")
        .arg(r)
        .arg("checkpoint")
        .env("GIT_DIR", other.path())
        .env("GIT_WORK_TREE", other.path())
        .env("GIT_INDEX_FILE", other.path().join("index"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.bare")
        .env("GIT_CONFIG_VALUE_0", "true")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!other.path().join("index").exists());
    assert_eq!(blame(r, "a.txt")[0]["pending"], false);
}

#[cfg(unix)]
#[test]
fn wrapper_records_partial_edits_on_sigterm_and_keeps_session() {
    use std::time::{Duration, Instant};
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human\n");
    ok(r, &["checkpoint"]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .arg("--root")
        .arg(r)
        .args([
            "run",
            "--harness",
            "shell",
            "--session",
            "session-1",
            "--",
            "sh",
            "-c",
            "printf 'robot\\n' > a.txt; touch ready; exec sleep 20",
        ])
        .current_dir(r)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !r.join("ready").exists() {
        assert!(start.elapsed() < Duration::from_secs(8));
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: child is still running; SIGTERM is a valid signal.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("wrapper did not forward SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(143));
    assert_eq!(blame(r, "a.txt")[0]["ai"], true);
    assert!(
        git(r, &["--git-dir=.aimark/repo", "log", "-1", "--format=%B"])
            .contains("AI-Session: session-1")
    );
}

#[cfg(unix)]
#[test]
fn ledger_does_not_run_inherited_commit_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let r = d.path();
    let hooks = r.join("hooks");
    fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-commit");
    fs::write(&hook, "#!/bin/sh\ntouch hook-executed\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    write(r, "a.txt", "human\n");
    let o = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .arg("--root")
        .arg(r)
        .arg("checkpoint")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", &hooks)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!r.join("hook-executed").exists());
}

#[cfg(unix)]
#[test]
fn shadow_commit_hooks_stay_disabled_with_relative_git_paths() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new().unwrap();
    let root = d.path();
    write(root, "a.txt", "human\n");
    ok(root, &["checkpoint"]);
    let hooks = root.join(".aimark/repo/hooks");
    fs::create_dir_all(&hooks).unwrap();
    let pre_commit = hooks.join("pre-commit");
    fs::write(&pre_commit, "#!/bin/sh\ntouch hook-executed\nexit 88\n").unwrap();
    fs::set_permissions(&pre_commit, fs::Permissions::from_mode(0o755)).unwrap();
    write(root, "a.txt", "changed human line\n");
    ok(root, &["checkpoint"]);
    assert!(!root.join("hook-executed").exists());
    assert_eq!(blame(root, "a.txt")[0]["pending"], false);
}

#[test]
fn claude_checkpoint_failure_returns_blocking_exit_status() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human\n");
    ok(r, &["checkpoint"]);
    fs::write(
        r.join(".aimark/repo/index.lock"),
        "held by another Git operation",
    )
    .unwrap();
    let output = hook(r, "PreToolUse", Some(&r.join("a.txt")));
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn unscoped_record_handles_directory_replaced_by_symlink() {
    use std::os::unix::fs::symlink;
    let d = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let r = d.path();
    fs::create_dir(r.join("dir")).unwrap();
    write(r, "dir/a.txt", "human\n");
    write(outside.path(), "external.txt", "private\n");
    ok(r, &["checkpoint"]);
    fs::remove_file(r.join("dir/a.txt")).unwrap();
    fs::remove_dir(r.join("dir")).unwrap();
    symlink(outside.path(), r.join("dir")).unwrap();
    ok(r, &["record", "--harness", "test"]);
    let tree = git(r, &["--git-dir=.aimark/repo", "ls-tree", "-r", "HEAD"]);
    assert!(tree.contains("120000 blob"));
    assert!(!tree.contains("dir/a.txt"));
    let s: Value = serde_json::from_str(&ok(r, &["stat", "--json"])).unwrap();
    assert_eq!(s["total"], 0);
    assert_eq!(s["skipped"], 1);
}

#[test]
fn preserves_non_utf8_real_git_exclude_rules() {
    let d = TempDir::new().unwrap();
    let r = d.path();
    git(r, &["init", "-q"]);
    fs::create_dir_all(r.join(".git/info")).unwrap();
    let original = b"# comment: \xff\nlocal-private.txt\n";
    fs::write(r.join(".git/info/exclude"), original).unwrap();
    ok(r, &["init"]);
    let updated = fs::read(r.join(".git/info/exclude")).unwrap();
    assert!(updated.starts_with(original));
    write(r, "local-private.txt", "private\n");
    assert!(!git(r, &["status", "--porcelain"]).contains("local-private.txt"));
}

#[cfg(windows)]
#[test]
fn mixed_case_git_environment_cannot_redirect_windows_index() {
    let d = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let r = d.path();
    write(r, "a.txt", "human\n");
    let output = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .arg("--root")
        .arg(r)
        .arg("checkpoint")
        .env_remove("GIT_INDEX_FILE")
        .env("git_index_file", other.path().join("index"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!other.path().join("index").exists());
}

#[test]
fn hook_lock_timeout_blocks_without_changing_history() {
    use std::time::{Duration, Instant};
    let d = TempDir::new().unwrap();
    let root = d.path();
    write(root, "a.txt", "human\n");
    ok(root, &["checkpoint"]);
    let before = git(root, &["--git-dir=.aimark/repo", "rev-parse", "HEAD"]);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".aimark/lock"))
        .unwrap();
    lock.lock().unwrap();
    let start = Instant::now();
    let mut child = spawn_hook(root, "PreToolUse", Some(&root.join("a.txt")));
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(8) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("hook waited indefinitely for the ledger lock instead of blocking the edit");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("timed out waiting for ledger lock"));
    drop(lock);
    assert_eq!(
        git(root, &["--git-dir=.aimark/repo", "rev-parse", "HEAD"]),
        before
    );
}

#[cfg(unix)]
#[test]
fn stats_batches_file_queries_instead_of_repeating_them_for_every_file() {
    use std::os::unix::fs::PermissionsExt;
    let project = TempDir::new().unwrap();
    let helpers = TempDir::new().unwrap();
    let root = project.path();
    for i in 0..8 {
        write(root, &format!("file-{i}.txt"), "human\n");
    }
    ok(root, &["checkpoint"]);
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    let wrapper = helpers.path().join("git");
    fs::write(&wrapper, "#!/bin/sh\nprintf 'call\\n' >> \"$AIMARK_TEST_GIT_LOG\"\nexec \"$AIMARK_TEST_REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let log = helpers.path().join("calls");
    let output = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .arg("--root")
        .arg(root)
        .args(["stat", "--json"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                helpers.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("AIMARK_TEST_REAL_GIT", real_git.trim())
        .env("AIMARK_TEST_GIT_LOG", &log)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stats: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stats["total"], 8);
    let calls = fs::read_to_string(log).unwrap().lines().count();
    assert!(
        calls <= 11,
        "eight tracked text files spawned {calls} Git processes, expected at most one blame per file plus three batched reads"
    );
}
