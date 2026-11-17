use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

fn cli(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn payload(root: &Path, event: &str, id: &str, patch: &str) -> Value {
    json!({"hook_event_name":event,"tool_name":"apply_patch","tool_use_id":id,"session_id":"session-test","turn_id":"turn-test","model":"example-model","cwd":root,"tool_input":{"command":patch}})
}
fn hook(root: &Path, value: &Value) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aimark"))
        .current_dir(root)
        .arg("--root")
        .arg(root)
        .args(["hook", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Err(error) = child
        .stdin
        .take()
        .unwrap()
        .write_all(value.to_string().as_bytes())
    {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().unwrap()
}
fn ok(root: &Path, value: &Value) {
    let output = hook(root, value);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "Codex hook success must not add model context"
    );
}
fn blame(root: &Path, file: &str) -> Value {
    serde_json::from_str(&cli(root, &["blame", file, "--json"])).unwrap()
}
fn write(root: &Path, file: &str, text: &str) {
    fs::write(root.join(file), text).unwrap();
}
fn head(root: &Path) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(["--git-dir=.aimark/repo", "rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn codex_records_multiple_patch_files_with_model_and_session() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    write(root, "a.txt", "human\n");
    write(root, "delete.txt", "remove me\n");
    write(root, "unrelated.txt", "old human\n");
    cli(root, &["checkpoint"]);
    write(root, "unrelated.txt", "pending human\n");
    let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n human\n+robot\n*** Add File: new.txt\n+new robot\n*** Delete File: delete.txt\n*** End Patch";
    ok(root, &payload(root, "PreToolUse", "multi", patch));
    write(root, "a.txt", "human\nrobot\n");
    write(root, "new.txt", "new robot\n");
    fs::remove_file(root.join("delete.txt")).unwrap();
    ok(root, &payload(root, "PostToolUse", "multi", patch));
    assert_eq!(blame(root, "a.txt")[0]["ai"], false);
    assert_eq!(blame(root, "a.txt")[1]["author"], "codex/example-model");
    assert_eq!(blame(root, "new.txt")[0]["ai"], true);
    assert_eq!(blame(root, "unrelated.txt")[0]["pending"], true);
    let output = Command::new("git")
        .current_dir(root)
        .args(["--git-dir=.aimark/repo", "log", "-1", "--format=%B"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("AI-Session: session-test"));
    assert_eq!(
        fs::read_dir(root.join(".aimark/pending")).unwrap().count(),
        0
    );
}

#[test]
fn codex_rename_resolves_paths_from_the_hook_working_directory() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    fs::create_dir(root.join("src")).unwrap();
    write(root, "src/old.txt", "human\n");
    cli(root, &["checkpoint"]);
    let patch = "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n@@\n human\n+robot\n*** End Patch";
    let mut pre = payload(&root.join("src"), "PreToolUse", "rename", patch);
    ok(root, &pre);
    fs::rename(root.join("src/old.txt"), root.join("src/new.txt")).unwrap();
    write(root, "src/new.txt", "human\nrobot\n");
    pre["hook_event_name"] = json!("PostToolUse");
    pre["model"] = json!("later-model");
    ok(root, &pre);
    assert_eq!(blame(root, "src/new.txt")[0]["ai"], false);
    assert_eq!(
        blame(root, "src/new.txt")[1]["author"],
        "codex/example-model"
    );
}

#[test]
fn codex_overlapping_patches_leave_each_others_baselines_alone() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    write(root, "a.txt", "human a\n");
    write(root, "b.txt", "human b\n");
    cli(root, &["checkpoint"]);
    let a = "*** Begin Patch\n*** Update File: a.txt\n@@\n-human a\n+robot a\n*** End Patch";
    let b = "*** Begin Patch\n*** Update File: b.txt\n@@\n-human b\n+robot b\n*** End Patch";
    ok(root, &payload(root, "PreToolUse", "a", a));
    write(root, "a.txt", "robot a\n");
    ok(root, &payload(root, "PreToolUse", "b", b));
    write(root, "b.txt", "robot b\n");
    ok(root, &payload(root, "PostToolUse", "b", b));
    ok(root, &payload(root, "PostToolUse", "a", a));
    assert_eq!(blame(root, "a.txt")[0]["ai"], true);
    assert_eq!(blame(root, "b.txt")[0]["ai"], true);
}

#[test]
fn codex_requires_a_matching_checkpoint_and_rejects_replayed_posts() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    write(root, "a.txt", "human\n");
    write(root, "b.txt", "human\n");
    cli(root, &["checkpoint"]);
    let a = "*** Begin Patch\n*** Update File: a.txt\n@@\n-human\n+robot\n*** End Patch";
    let b = "*** Begin Patch\n*** Update File: b.txt\n@@\n-human\n+robot\n*** End Patch";
    let before = head(root);
    let post = payload(root, "PostToolUse", "receipt", a);
    let output = hook(root, &post);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("matching pre-edit checkpoint"));
    assert_eq!(head(root), before);
    ok(root, &payload(root, "PreToolUse", "receipt", a));
    write(root, "a.txt", "robot\n");
    assert_eq!(
        hook(root, &payload(root, "PostToolUse", "receipt", b))
            .status
            .code(),
        Some(2)
    );
    ok(root, &post);
    write(root, "a.txt", "later human\n");
    assert_eq!(hook(root, &post).status.code(), Some(2));
    assert_eq!(blame(root, "a.txt")[0]["pending"], true);
}

#[test]
fn codex_source_lines_that_look_like_headers_do_not_select_other_files() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    write(root, "a.txt", "*** Update File: unrelated.txt\n");
    write(root, "unrelated.txt", "old human\n");
    cli(root, &["checkpoint"]);
    write(root, "unrelated.txt", "pending human\n");
    let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n *** Update File: unrelated.txt\n+*** Add File: fake.txt\n*** End Patch";
    ok(root, &payload(root, "PreToolUse", "source-markers", patch));
    write(
        root,
        "a.txt",
        "*** Update File: unrelated.txt\n*** Add File: fake.txt\n",
    );
    ok(root, &payload(root, "PostToolUse", "source-markers", patch));
    assert_eq!(blame(root, "unrelated.txt")[0]["pending"], true);
    assert_eq!(blame(root, "a.txt")[1]["ai"], true);
}

#[test]
fn codex_rejects_bad_payloads_and_paths_before_checkpointing() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    write(root, "a.txt", "human\n");
    cli(root, &["checkpoint"]);
    write(root, "a.txt", "pending human\n");
    let before = head(root);
    let valid = "*** Begin Patch\n*** Update File: a.txt\n@@\n-human\n+robot\n*** End Patch";
    let mut cases = Vec::new();
    let mut missing = payload(root, "PreToolUse", "bad", valid);
    missing["tool_input"] = json!({});
    cases.push(missing);
    let mut shell = payload(root, "PreToolUse", "bad", valid);
    shell["tool_name"] = json!("Bash");
    cases.push(shell);
    let mut no_id = payload(root, "PreToolUse", "bad", valid);
    no_id["tool_use_id"] = Value::Null;
    cases.push(no_id);
    cases.push(payload(root, "Stop", "bad", valid));
    for patch in [
        "*** Begin Patch\n*** End Patch",
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-human\n+robot",
        "*** Begin Patch\n*** Environment ID: remote\n*** Add File: a.txt\n+robot\n*** End Patch",
    ] {
        cases.push(payload(root, "PreToolUse", "bad", patch));
    }
    for path in [
        "../outside.txt",
        ".aimark/repo/HEAD",
        ".git/config",
        ":(glob)**",
    ] {
        cases.push(payload(
            root,
            "PreToolUse",
            "bad",
            &format!("*** Begin Patch\n*** Add File: {path}\n+robot\n*** End Patch"),
        ));
    }
    for value in cases {
        let output = hook(root, &value);
        assert_eq!(output.status.code(), Some(2), "accepted {value}");
        assert_eq!(head(root), before);
    }
    assert_eq!(blame(root, "a.txt")[0]["pending"], true);
}

#[test]
fn codex_blocks_a_second_inflight_patch_to_the_same_file() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    write(root, "a.txt", "human\n");
    cli(root, &["checkpoint"]);
    let patch_a = "*** Begin Patch\n*** Update File: a.txt\n@@\n human\n+robot A\n*** End Patch";
    let patch_b = "*** Begin Patch\n*** Update File: a.txt\n@@\n robot A\n+robot B\n*** End Patch";
    let mut pre_a = payload(root, "PreToolUse", "first", patch_a);
    pre_a["model"] = json!("model-a");
    ok(root, &pre_a);
    write(root, "a.txt", "human\nrobot A\n");
    let before_second = head(root);
    let mut pre_b = payload(root, "PreToolUse", "second", patch_b);
    pre_b["session_id"] = json!("another-session");
    pre_b["model"] = json!("model-b");
    let output = hook(root, &pre_b);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("another Codex patch is still pending")
    );
    assert_eq!(head(root), before_second);
    pre_a["hook_event_name"] = json!("PostToolUse");
    ok(root, &pre_a);
    ok(root, &pre_b);
    write(root, "a.txt", "human\nrobot A\nrobot B\n");
    pre_b["hook_event_name"] = json!("PostToolUse");
    ok(root, &pre_b);
    let lines = blame(root, "a.txt");
    assert_eq!(lines[1]["author"], "codex/model-a");
    assert_eq!(lines[2]["author"], "codex/model-b");
}

#[test]
fn codex_failed_record_keeps_its_file_scope_blocked() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    write(root, "a.txt", "human\n");
    cli(root, &["checkpoint"]);
    let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-human\n+robot\n*** End Patch";
    ok(root, &payload(root, "PreToolUse", "failed-record", patch));
    write(root, "a.txt", "robot\n");
    let before = head(root);
    fs::write(root.join(".aimark/repo/index.lock"), "held").unwrap();
    assert_eq!(
        hook(root, &payload(root, "PostToolUse", "failed-record", patch))
            .status
            .code(),
        Some(2)
    );
    fs::remove_file(root.join(".aimark/repo/index.lock")).unwrap();
    let next = hook(root, &payload(root, "PreToolUse", "retry", patch));
    assert_eq!(next.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&next.stderr).contains("another Codex patch is still pending"));
    assert_eq!(head(root), before);
    assert_eq!(
        hook(root, &payload(root, "PostToolUse", "failed-record", patch))
            .status
            .code(),
        Some(2)
    );
    assert_eq!(blame(root, "a.txt")[0]["pending"], true);
}
