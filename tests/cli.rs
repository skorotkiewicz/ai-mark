use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
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
fn hook(root: &Path, event: &str, file: Option<&Path>) -> Output {
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
    p.wait_with_output().unwrap()
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
    assert!(
        hook(r, "PreToolUse", Some(&r.join("a.txt")))
            .status
            .success()
    );
    write(r, "a.txt", "robot a\n");
    assert!(
        hook(r, "PreToolUse", Some(&r.join("b.txt")))
            .status
            .success()
    );
    write(r, "b.txt", "robot b\n");
    assert!(
        hook(r, "PostToolUse", Some(&r.join("a.txt")))
            .status
            .success()
    );
    assert!(
        hook(r, "PostToolUse", Some(&r.join("b.txt")))
            .status
            .success()
    );
    assert_eq!(blame(r, "a.txt")[0]["ai"], true);
    assert_eq!(blame(r, "b.txt")[0]["ai"], true);
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
    let o = cli(
        &r.join("src"),
        &[
            "run",
            "--harness",
            "shell",
            "--",
            "sh",
            "-c",
            "printf 'robot\n' > a.txt; exit 7",
        ],
    );
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
