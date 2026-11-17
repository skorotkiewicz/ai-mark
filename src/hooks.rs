use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::Path,
};

pub struct ClaudeHook {
    pub event: String,
    pub files: Vec<String>,
    pub model: Option<String>,
    pub session: Option<String>,
}

pub fn read_payload() -> Result<Value> {
    let mut input = String::new();
    io::stdin().take(4_194_305).read_to_string(&mut input)?;
    ensure!(input.len() <= 4_194_304, "hook payload exceeds 4 MiB");
    serde_json::from_str(&input).context("hook stdin must be JSON")
}

pub fn read_claude() -> Result<ClaudeHook> {
    let value = read_payload()?;
    let tool = value["tool_name"].as_str().unwrap_or("");
    ensure!(
        ["Edit", "Write", "MultiEdit", "NotebookEdit"].contains(&tool),
        "unsupported Claude file tool: {tool}"
    );
    let path = value["tool_input"]["file_path"]
        .as_str()
        .or_else(|| value["tool_input"]["notebook_path"].as_str())
        .filter(|p| !p.is_empty())
        .context(
            "hook payload has no file_path or notebook_path; refusing to record the whole project",
        )?;
    let model = value["model"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| transcript_model(Path::new(value["transcript_path"].as_str()?)));
    Ok(ClaudeHook {
        event: value["hook_event_name"].as_str().unwrap_or("").into(),
        files: vec![path.into()],
        model,
        session: value["session_id"].as_str().map(str::to_owned),
    })
}

fn transcript_model(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut model = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(value) = serde_json::from_str::<Value>(&line)
            && let Some(name) = value["message"]["model"]
                .as_str()
                .filter(|m| !m.starts_with('<'))
        {
            model = Some(name.into());
        }
    }
    model
}
