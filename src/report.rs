use crate::ledger::{AI_MAIL, Ledger};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{fs, io::Write, path::Path, process::Stdio};

pub const MAX_TEXT_BYTES: u64 = 1_000_000;

#[derive(Serialize)]
pub struct Line {
    pub line: usize,
    pub ai: bool,
    pub pending: bool,
    pub author: String,
    pub text: String,
    pub commit: Option<String>,
}

#[derive(Serialize)]
pub struct FileStat {
    pub file: String,
    pub ai: usize,
    pub human: usize,
    pub pending: usize,
    pub total: usize,
    pub pct: f64,
}

#[derive(Serialize)]
pub struct Stats {
    pub files: Vec<FileStat>,
    pub ai: usize,
    pub human: usize,
    pub pending: usize,
    pub total: usize,
    pub pct: f64,
    pub skipped: usize,
}

fn text(ledger: &Ledger, file: &str) -> Result<String> {
    ledger.path(Path::new(file), &ledger.root)?;
    let path = ledger.root.join(file);
    let meta = fs::symlink_metadata(&path).context("file does not exist")?;
    ensure!(
        meta.is_file() && meta.len() <= MAX_TEXT_BYTES,
        "only text files up to {MAX_TEXT_BYTES} bytes can be inspected"
    );
    let bytes = fs::read(&path)?;
    ensure!(
        bytes.len() as u64 <= MAX_TEXT_BYTES && !bytes.contains(&0),
        "file is binary or too large"
    );
    String::from_utf8(bytes).context("file is not UTF-8 text")
}

pub fn blame(ledger: &Ledger, file: &str) -> Result<Vec<Line>> {
    ensure!(
        ledger.files(&[file.to_owned()])?.iter().any(|f| f == file),
        "file is ignored or unavailable"
    );
    let contents = text(ledger, file)?;
    let repository = gix::open_opts(
        ledger.root.join(".aimark/repo"),
        gix::open::Options::isolated(),
    )?;
    let tree = repository.head_commit()?.tree()?;
    if tree.lookup_entry_by_path(file)?.is_none() {
        return Ok(contents
            .lines()
            .enumerate()
            .map(|(i, t)| Line {
                line: i + 1,
                ai: false,
                pending: true,
                author: "human".into(),
                text: t.into(),
                commit: None,
            })
            .collect());
    }
    let mut child = ledger
        .git()
        .args([
            "blame",
            "--line-porcelain",
            "-w",
            "-M",
            "--contents",
            "-",
            "--",
            file,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("Git stdin unavailable")?
        .write_all(contents.as_bytes())?;
    let result = child.wait_with_output()?;
    ensure!(
        result.status.success(),
        "Git blame failed: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    parse_blame(&String::from_utf8(result.stdout).context("Git blame was not UTF-8")?)
}

fn parse_blame(porcelain: &str) -> Result<Vec<Line>> {
    let mut rows = Vec::new();
    let (mut number, mut author, mut email, mut sha) =
        (0, String::new(), String::new(), String::new());
    for line in porcelain.lines() {
        if let Some(source) = line.strip_prefix('\t') {
            let pending = sha.chars().all(|c| c == '0');
            let ai = !pending && email == format!("<{AI_MAIL}>");
            rows.push(Line {
                line: number,
                ai,
                pending,
                author: if ai { author.clone() } else { "human".into() },
                text: source.into(),
                commit: if pending { None } else { Some(sha.clone()) },
            });
        } else if let Some(value) = line.strip_prefix("author ") {
            author = value.into();
        } else if let Some(value) = line.strip_prefix("author-mail ") {
            email = value.into();
        } else {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() >= 3
                && matches!(fields[0].len(), 40 | 64)
                && fields[0].chars().all(|c| c.is_ascii_hexdigit())
            {
                sha = fields[0].into();
                number = fields[2].parse().context("invalid blame line number")?;
            }
        }
    }
    Ok(rows)
}

pub fn stats(ledger: &Ledger) -> Result<Stats> {
    let mut result = Stats {
        files: Vec::new(),
        ai: 0,
        human: 0,
        pending: 0,
        total: 0,
        pct: 0.0,
        skipped: 0,
    };
    for file in ledger.files(&[])? {
        if !ledger.root.join(&file).exists() {
            continue;
        }
        let rows = match blame(ledger, &file) {
            Ok(rows) => rows,
            Err(_) if text(ledger, &file).is_err() => {
                result.skipped += 1;
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("could not inspect {file}")),
        };
        let total = rows.len();
        let ai = rows.iter().filter(|l| l.ai).count();
        let pending = rows.iter().filter(|l| l.pending).count();
        result.ai += ai;
        result.total += total;
        result.pending += pending;
        result.files.push(FileStat {
            file,
            ai,
            human: total - ai,
            pending,
            total,
            pct: percentage(ai, total),
        });
    }
    result.human = result.total - result.ai;
    result.pct = percentage(result.ai, result.total);
    result
        .files
        .sort_by(|a, b| b.pct.total_cmp(&a.pct).then(a.file.cmp(&b.file)));
    Ok(result)
}

fn percentage(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * part as f64 / total as f64
    }
}
