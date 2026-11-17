use crate::{
    hooks,
    ledger::{Ledger, validate_metadata},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, hash_map::DefaultHasher},
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    io::{Read, Write},
    path::{Path, PathBuf},
};

struct Hook {
    event: String,
    session: String,
    turn: String,
    tool_use: String,
    model: String,
    cwd: PathBuf,
    files: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    session: String,
    turn: String,
    tool_use: String,
    model: String,
    files: Vec<String>,
}

pub fn handle(root: PathBuf) -> Result<()> {
    let hook = parse_hook(hooks::read_payload()?)?;
    let ledger = Ledger::open(root)?;
    let files = hook
        .files
        .iter()
        .map(|file| ledger.path(file, &hook.cwd))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let directory = ledger.root.join(".aimark/pending");
    let mut hash = DefaultHasher::new();
    (&hook.session, &hook.turn, &hook.tool_use).hash(&mut hash);
    let path = directory.join(format!("codex-{:016x}.json", hash.finish()));
    let recording = path.with_extension("recording.json");

    if hook.event == "PreToolUse" {
        if !directory.exists() {
            fs::create_dir(&directory)?;
        }
        ensure_directory(&directory)?;
        ensure!(
            [&path, &recording]
                .iter()
                .all(|path| fs::symlink_metadata(path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)),
            "a Codex checkpoint already exists for this tool call"
        );
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name
                .to_str()
                .is_some_and(|name| name.starts_with("codex-") && name.ends_with(".json"))
            {
                continue;
            }
            let pending = read_receipt(&entry.path())?;
            ensure!(
                !files.iter().any(|file| pending
                    .files
                    .iter()
                    .any(|other| scopes_overlap(file, other))),
                "another Codex patch is still pending for an overlapping file; let it finish before editing that file"
            );
        }
        ledger.checkpoint(&files)?;
        let receipt = Receipt {
            session: hook.session,
            turn: hook.turn,
            tool_use: hook.tool_use,
            model: hook.model,
            files,
        };
        let bytes = serde_json::to_vec(&receipt)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .context("could not save Codex checkpoint receipt")?;
        file.write_all(&bytes)
            .context("could not save Codex checkpoint receipt")?;
    } else {
        ensure_directory(&directory)
            .context("no matching pre-edit checkpoint for this Codex tool call")?;
        let receipt = read_receipt(&path)
            .context("no matching pre-edit checkpoint for this Codex tool call")?;
        ensure!(
            receipt.session == hook.session
                && receipt.turn == hook.turn
                && receipt.tool_use == hook.tool_use
                && receipt.files == files,
            "Codex post hook does not match its pre-edit checkpoint"
        );
        // Claim before recording so replayed post hooks cannot reuse the baseline. Keep
        // this scope reserved on failure, so a later pre hook cannot relabel leftover AI
        // changes as human. A successful record releases it.
        ensure!(
            fs::symlink_metadata(&recording)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "Codex checkpoint recording was already attempted"
        );
        fs::rename(&path, &recording)?;
        ledger.record("codex", &receipt.model, Some(&receipt.session), &files)?;
        fs::remove_file(recording)?;
    }
    Ok(())
}

fn read_receipt(path: &Path) -> Result<Receipt> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "invalid Codex checkpoint receipt"
    );
    let mut text = String::new();
    fs::File::open(path)?
        .take(4_194_305)
        .read_to_string(&mut text)?;
    ensure!(
        text.len() <= 4_194_304,
        "Codex checkpoint receipt exceeds 4 MiB"
    );
    serde_json::from_str(&text)
        .with_context(|| format!("invalid Codex checkpoint receipt: {}", path.display()))
}

fn scopes_overlap(left: &str, right: &str) -> bool {
    let normalize = |path: &str| {
        if cfg!(windows) {
            path.to_lowercase()
        } else {
            path.to_owned()
        }
    };
    let left = normalize(left);
    let right = normalize(right);
    left == right
        || left
            .strip_prefix(&right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(&left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn ensure_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Codex checkpoint directory must be an ordinary directory"
    );
    Ok(())
}

fn parse_hook(value: Value) -> Result<Hook> {
    let field = |key: &str| -> Result<String> {
        let text = value[key]
            .as_str()
            .with_context(|| format!("Codex hook is missing {key}"))?;
        validate_metadata(text)?;
        Ok(text.to_owned())
    };
    let event = field("hook_event_name")?;
    ensure!(
        ["PreToolUse", "PostToolUse"].contains(&event.as_str()),
        "unsupported Codex hook event: {event}"
    );
    ensure!(
        value["tool_name"].as_str() == Some("apply_patch"),
        "Codex integration only supports apply_patch; use the wrapper for shell writes"
    );
    let session = field("session_id")?;
    let turn = field("turn_id")?;
    let tool_use = field("tool_use_id")?;
    let model = value["model"].as_str().unwrap_or("unknown").to_owned();
    validate_metadata(&model)?;
    let cwd = PathBuf::from(value["cwd"].as_str().context("Codex hook is missing cwd")?);
    ensure!(
        cwd.is_absolute() && cwd.is_dir(),
        "Codex hook cwd must be an existing absolute local directory"
    );
    let command = value["tool_input"]["command"]
        .as_str()
        .context("Codex hook is missing tool_input.command")?;
    let files = patch_files(command)?;
    Ok(Hook {
        event,
        session,
        turn,
        tool_use,
        model,
        cwd,
        files,
    })
}

#[derive(Clone, Copy)]
enum Mode {
    Start,
    Add,
    Delete,
    Update { can_move: bool },
}

/// Extract targets using Codex 0.169's header trimming and context-line rules.
/// Codex validates the actual diff; this parser never executes or applies it.
fn patch_files(command: &str) -> Result<Vec<PathBuf>> {
    let mut lines: Vec<_> = command.trim().lines().collect();
    if lines.len() >= 4
        && ["<<EOF", "<<'EOF'", "<<\"EOF\""].contains(&lines[0])
        && lines.last().is_some_and(|line| line.ends_with("EOF"))
    {
        lines = lines[1..lines.len() - 1].to_vec();
    }
    ensure!(
        lines.len() >= 3
            && lines[0].trim() == "*** Begin Patch"
            && lines
                .last()
                .is_some_and(|line| line.trim() == "*** End Patch"),
        "Codex tool_input.command must contain a complete apply_patch patch"
    );
    let mut mode = Mode::Start;
    let mut paths = Vec::new();
    for line in &lines[1..lines.len() - 1] {
        let marker = if matches!(mode, Mode::Update { .. }) {
            line.trim_end()
        } else {
            line.trim()
        };
        ensure!(
            !marker.starts_with("*** Environment ID:"),
            "remote-environment patches are not supported by the local Codex integration"
        );
        let header = [
            ("*** Add File: ", Mode::Add),
            ("*** Delete File: ", Mode::Delete),
            ("*** Update File: ", Mode::Update { can_move: true }),
        ]
        .into_iter()
        .find_map(|(prefix, next)| marker.strip_prefix(prefix).map(|path| (path, next)));
        if let Some((path, next)) = header {
            push_path(&mut paths, path)?;
            mode = next;
            continue;
        }
        if let Some(path) = marker.strip_prefix("*** Move to: ") {
            ensure!(
                matches!(mode, Mode::Update { can_move: true }),
                "Codex move target must immediately follow an update header"
            );
            push_path(&mut paths, path)?;
            mode = Mode::Update { can_move: false };
            continue;
        }
        match mode {
            Mode::Add => ensure!(
                line.starts_with('+'),
                "invalid line in Codex add-file patch"
            ),
            Mode::Update { .. } => {
                ensure!(
                    line.is_empty()
                        || line.starts_with([' ', '+', '-'])
                        || marker == "@@"
                        || marker.starts_with("@@ ")
                        || marker == "*** End of File",
                    "invalid line in Codex update patch"
                );
                mode = Mode::Update { can_move: false };
            }
            _ => bail!("invalid Codex patch header"),
        }
    }
    ensure!(
        !paths.is_empty(),
        "Codex patch has no target files; refusing to record the whole project"
    );
    Ok(paths)
}

fn push_path(paths: &mut Vec<PathBuf>, path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && !path.contains("://"),
        "Codex patch target must be a local file path"
    );
    paths.push(PathBuf::from(path));
    Ok(())
}
