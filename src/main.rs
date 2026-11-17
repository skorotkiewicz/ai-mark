use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

const AI_MAIL: &str = "ai@aimark.local";
const AI_MAIL_PORCELAIN: &str = "<ai@aimark.local>";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

#[derive(Parser)]
#[command(
    name = "aimark",
    version,
    about = "Line-level AI provenance via a shadow git ledger"
)]
struct Cli {
    /// Project root (defaults to cwd)
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the shadow ledger under .aimark/
    Init,
    /// Commit current state as human. Call BEFORE an AI edit.
    Checkpoint,
    /// Commit current state as AI. Call AFTER an AI edit.
    Record {
        #[arg(long)]
        harness: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        session: Option<String>,
        /// Only attribute these files to AI (comma separated)
        #[arg(long, value_delimiter = ',')]
        files: Vec<String>,
        /// Read hook JSON from stdin. Supported: "claude"
        #[arg(long)]
        stdin: Option<String>,
    },
    /// Universal adapter: checkpoint, run a harness, attribute the diff to AI
    Run {
        #[arg(long)]
        harness: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        cmd: Vec<String>,
    },
    /// Like git blame, but authors are "human" or "harness/model"
    Blame {
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// AI line percentage per file
    Stat {
        #[arg(long)]
        json: bool,
    },
}

// Strip inherited GIT_* vars so we behave correctly inside hooks
fn clean_git() -> Command {
    let mut c = Command::new("git");
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ] {
        c.env_remove(k);
    }
    c
}

fn shadow(root: &Path) -> Command {
    let mut c = clean_git();
    c.arg("--git-dir")
        .arg(root.join(".aimark/repo"))
        .arg("--work-tree")
        .arg(root)
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("-c")
        .arg("core.quotepath=false")
        .current_dir(root);
    c
}

fn out(mut c: Command) -> Result<String> {
    let o = c.output().context("failed to run git, is it installed?")?;
    if !o.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn init(root: &Path) -> Result<()> {
    let repo = root.join(".aimark/repo");
    fs::create_dir_all(root.join(".aimark"))?;
    if !repo.join("HEAD").exists() {
        // Bare dir + explicit --work-tree, the classic dotfiles-repo pattern
        out(clean_git().current_dir(root).args([
            "init",
            "-q",
            "--bare",
            "--initial-branch=main",
            ".aimark/repo",
        ]))?;
        for (k, v) in [
            ("user.name", "aimark"),
            ("user.email", "aimark@local"),
            ("commit.gpgsign", "false"),
            ("core.bare", "false"),
        ] {
            out(shadow(root).args(["config", k, v]))?;
        }
        fs::create_dir_all(repo.join("info"))?;
        // The worktree's own .gitignore files are honored automatically
        fs::write(repo.join("info/exclude"), "/.git/\n/.aimark/\n")?;
    }
    // Hide the ledger from the project's real git status, locally only
    let pg = root.join(".git");
    if pg.is_dir() {
        let ex = pg.join("info/exclude");
        fs::create_dir_all(ex.parent().unwrap())?;
        let cur = fs::read_to_string(&ex).unwrap_or_default();
        if !cur.lines().any(|l| l.trim() == ".aimark/") {
            fs::write(&ex, format!("{}\n.aimark/\n", cur.trim_end_matches('\n')))?;
        }
    }
    Ok(())
}

fn human(root: &Path) -> (String, String) {
    let get = |k: &str| {
        out(clean_git().current_dir(root).args(["config", k]))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let name = get("user.name")
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "human".into());
    let mail = get("user.email").unwrap_or_else(|| "human@local".into());
    (name, mail)
}

fn staged(root: &Path) -> Result<bool> {
    let has_head = shadow(root)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()?
        .status
        .success();
    if has_head {
        Ok(shadow(root)
            .args(["diff", "--cached", "--quiet"])
            .status()?
            .code()
            == Some(1))
    } else {
        Ok(!out(shadow(root).args(["ls-files"]))?.trim().is_empty())
    }
}

fn commit(root: &Path, name: &str, mail: &str, msg: &str, pathspecs: &[String]) -> Result<bool> {
    let mut add = shadow(root);
    add.args(["add", "-A", "--"]).args(pathspecs);
    out(add)?;
    if !staged(root)? {
        return Ok(false);
    }
    out(shadow(root)
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", mail)
        .args(["commit", "-q", "-m", msg]))?;
    Ok(true)
}

fn checkpoint(root: &Path) -> Result<()> {
    init(root)?;
    let (n, m) = human(root);
    commit(root, &n, &m, "human checkpoint", &[".".into()])?;
    Ok(())
}

fn relativize(root: &Path, f: &str) -> Option<String> {
    let p = Path::new(f);
    if p.is_absolute() {
        p.strip_prefix(root)
            .ok()
            .map(|r| r.to_string_lossy().into_owned())
    } else {
        Some(f.trim_start_matches("./").to_string())
    }
}

// Claude hook JSON has no model field, but the transcript does
fn transcript_model(path: Option<&str>) -> Option<String> {
    let text = fs::read_to_string(path?).ok()?;
    let mut model = None;
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(m) = v["message"]["model"].as_str() {
                model = Some(m.to_string());
            }
        }
    }
    model
}

fn record(
    root: &Path,
    harness: &str,
    mut model: Option<String>,
    mut session: Option<String>,
    mut files: Vec<String>,
    stdin: Option<String>,
) -> Result<()> {
    init(root)?;
    if let Some(kind) = stdin {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        let v: Value = serde_json::from_str(&buf).context("hook stdin was not JSON")?;
        match kind.as_str() {
            "claude" => {
                if files.is_empty() {
                    if let Some(p) = v["tool_input"]["file_path"]
                        .as_str()
                        .or_else(|| v["tool_input"]["notebook_path"].as_str())
                    {
                        files.push(p.to_string());
                    }
                }
                if session.is_none() {
                    session = v["session_id"].as_str().map(Into::into);
                }
                if model.is_none() {
                    model = transcript_model(v["transcript_path"].as_str());
                }
            }
            other => bail!("unknown stdin adapter: {other}"),
        }
    }
    let model = model.unwrap_or_else(|| "unknown".into());
    let author = format!("{harness}/{model}");
    let msg = format!(
        "ai edit: {harness}\n\nAI-Harness: {harness}\nAI-Model: {model}\nAI-Session: {}",
        session.as_deref().unwrap_or("-")
    );
    let rels: Vec<String> = files.iter().filter_map(|f| relativize(root, f)).collect();
    if !files.is_empty() && rels.is_empty() {
        return Ok(()); // edit happened outside this project
    }
    let changed = if rels.is_empty() {
        // No file list: attribute the whole diff (wrapper semantics)
        commit(root, &author, AI_MAIL, &msg, &[".".into()])?
    } else {
        // Split: everything except the AI's files is a human checkpoint
        let (hn, hm) = human(root);
        let mut human_specs = vec![".".to_string()];
        for f in &rels {
            human_specs.push(format!(":(exclude,literal){f}"));
        }
        commit(root, &hn, &hm, "human checkpoint", &human_specs)?;
        commit(root, &author, AI_MAIL, &msg, &rels)?
    };
    if changed {
        let sha = out(shadow(root).args(["rev-parse", "--short", "HEAD"]))?;
        println!("aimark: {} {}", sha.trim(), author);
    }
    Ok(())
}

struct Bl {
    no: usize,
    ai: bool,
    author: String,
    text: String,
}

fn blame(root: &Path, file: &str) -> Result<Vec<Bl>> {
    // -w: whitespace tolerance (formatter drift), -M: moved lines
    let o = shadow(root)
        .args(["blame", "--line-porcelain", "-w", "-M", "--", file])
        .output()?;
    if !o.status.success() {
        bail!("{}", String::from_utf8_lossy(&o.stderr).trim());
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let mut res = Vec::new();
    let (mut no, mut author, mut mail) = (0usize, String::new(), String::new());
    for line in text.lines() {
        if let Some(code) = line.strip_prefix('\t') {
            res.push(Bl {
                no,
                ai: mail == AI_MAIL_PORCELAIN,
                author: author.clone(),
                text: code.to_string(),
            });
        } else if let Some(a) = line.strip_prefix("author ") {
            author = a.to_string();
        } else if let Some(m) = line.strip_prefix("author-mail ") {
            mail = m.to_string();
        } else {
            let t: Vec<&str> = line.split(' ').collect();
            if t.len() >= 3 {
                let sha = t[0].trim_start_matches('^'); // boundary commits
                if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    no = t[2].parse().unwrap_or(0);
                }
            }
        }
    }
    Ok(res)
}

fn color_on() -> bool {
    io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn looks_text(p: &Path) -> bool {
    let Ok(mut f) = fs::File::open(p) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    !buf[..n].contains(&0)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.canonicalize().context("invalid --root")?;
    match cli.cmd {
        Cmd::Init => {
            init(&root)?;
            println!("ledger ready at {}", root.join(".aimark").display());
        }
        Cmd::Checkpoint => checkpoint(&root)?,
        Cmd::Record {
            harness,
            model,
            session,
            files,
            stdin,
        } => {
            record(&root, &harness, model, session, files, stdin)?;
        }
        Cmd::Run {
            harness,
            model,
            cmd,
        } => {
            checkpoint(&root)?;
            let st = Command::new(&cmd[0])
                .args(&cmd[1..])
                .current_dir(&root)
                .status()
                .with_context(|| format!("failed to spawn {}", cmd[0]))?;
            record(&root, &harness, model, None, vec![], None)?;
            std::process::exit(st.code().unwrap_or(1));
        }
        Cmd::Blame { file, json } => {
            checkpoint(&root)?; // unrecorded state counts as human
            let rel = relativize(&root, &file.to_string_lossy())
                .context("file is outside the project")?;
            let lines = blame(&root, &rel)?;
            if json {
                let arr: Vec<Value> = lines
                    .iter()
                    .map(|l| json!({"line": l.no, "ai": l.ai, "author": l.author, "text": l.text}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&arr)?);
            } else {
                let color = color_on();
                for l in &lines {
                    let who = if l.ai { l.author.as_str() } else { "human" };
                    let row = format!("{:>5}  {:<24}  {}", l.no, who, l.text);
                    if color && l.ai {
                        println!("{GREEN}{row}{RESET}");
                    } else {
                        println!("{row}");
                    }
                }
            }
        }
        Cmd::Stat { json } => {
            checkpoint(&root)?;
            let files = out(shadow(&root).args(["ls-files"]))?;
            let mut rows: Vec<(String, usize, usize)> = Vec::new();
            let (mut tai, mut ttot) = (0usize, 0usize);
            for f in files.lines() {
                let p = root.join(f);
                let Ok(md) = fs::metadata(&p) else { continue };
                if md.len() > 1_000_000 || !looks_text(&p) {
                    continue;
                }
                let Ok(ls) = blame(&root, f) else { continue };
                let tot = ls.len();
                if tot == 0 {
                    continue;
                }
                let ai = ls.iter().filter(|l| l.ai).count();
                tai += ai;
                ttot += tot;
                rows.push((f.to_string(), ai, tot));
            }
            rows.sort_by(|a, b| {
                let pa = a.1 as f64 / a.2 as f64;
                let pb = b.1 as f64 / b.2 as f64;
                pb.partial_cmp(&pa).unwrap()
            });
            if json {
                let arr: Vec<Value> = rows
                    .iter()
                    .map(|(f, ai, tot)| json!({"file": f, "ai": ai, "total": tot}))
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "files": arr, "ai": tai, "total": ttot,
                        "pct": if ttot > 0 { 100.0 * tai as f64 / ttot as f64 } else { 0.0 }
                    }))?
                );
            } else {
                let color = color_on();
                for (f, ai, tot) in &rows {
                    let pct = 100.0 * *ai as f64 / *tot as f64;
                    let row = format!("{pct:>6.1}%  {ai:>5}/{tot:<5}  {f}");
                    if color && *ai > 0 {
                        println!("{GREEN}{row}{RESET}");
                    } else {
                        println!("{row}");
                    }
                }
                let pct = if ttot > 0 {
                    100.0 * tai as f64 / ttot as f64
                } else {
                    0.0
                };
                println!("{pct:>6.1}%  {tai:>5}/{ttot:<5}  TOTAL");
            }
        }
    }
    Ok(())
}
