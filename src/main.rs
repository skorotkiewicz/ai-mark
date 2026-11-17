mod codex;
mod dashboard;
mod hooks;
mod ledger;
mod report;
mod runner;
mod snapshot;

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use ledger::{Ledger, validate_metadata};
use std::{
    ffi::OsString,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(version, about = "Local line-level human and AI code provenance")]
struct Cli {
    /// Project root; otherwise discover from the current directory
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Args)]
struct Attribution {
    #[arg(long, value_parser = metadata)]
    harness: String,
    #[arg(long, value_parser = metadata)]
    model: Option<String>,
    #[arg(long, value_parser = metadata)]
    session: Option<String>,
}

#[derive(Subcommand)]
enum Action {
    /// Initialize the independent .aimark ledger
    Init,
    /// Snapshot human changes BEFORE an AI edit
    Checkpoint {
        /// Limit to a literal path; repeat for multiple paths
        #[arg(long)]
        files: Vec<PathBuf>,
    },
    /// Snapshot AI changes AFTER an edit
    Record {
        #[command(flatten)]
        attribution: Attribution,
        /// Attribute only this literal path; repeat for multiple paths
        #[arg(long)]
        files: Vec<PathBuf>,
        /// Read a Claude hook payload from stdin
        #[arg(long, value_parser = ["claude"])]
        stdin: Option<String>,
    },
    /// Checkpoint, run a command, then record its edits, including on failure
    Run {
        #[command(flatten)]
        attribution: Attribution,
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<OsString>,
    },
    /// Inspect current lines without committing pending changes
    Blame {
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Report AI line percentages without committing pending changes
    Stat {
        #[arg(long)]
        json: bool,
    },
    /// Serve a local browser dashboard, with no external services
    Dashboard {
        /// Loopback HTTP port; 0 chooses an available port
        #[arg(long, default_value_t = 4242)]
        port: u16,
    },
    /// Handle scoped Claude or Codex hook JSON from stdin
    Hook {
        #[arg(value_parser = ["claude", "codex"])]
        adapter: String,
    },
}

fn metadata(input: &str) -> std::result::Result<String, String> {
    validate_metadata(input).map_err(|e| e.to_string())?;
    Ok(input.into())
}
fn paths(ledger: &Ledger, input: &[PathBuf], base: &Path) -> Result<Vec<String>> {
    input.iter().map(|p| ledger.path(p, base)).collect()
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let hook = matches!(&cli.command, Action::Hook { .. });
    match execute(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("aimark: {error:#}");
            // Claude and Codex interpret exit 2 as blocking for supported PreToolUse hooks.
            std::process::ExitCode::from(if hook { 2 } else { 1 })
        }
    }
}

fn execute(cli: Cli) -> Result<std::process::ExitCode> {
    let cwd = std::env::current_dir()?.canonicalize()?;
    let root = ledger::discover(cli.root.as_deref(), &cwd)?;
    let base = if cli.root.is_some() { &root } else { &cwd };
    if matches!(&cli.command, Action::Hook { adapter } if adapter == "codex") {
        codex::handle(root)?;
        return Ok(std::process::ExitCode::SUCCESS);
    }
    if let Action::Run {
        attribution,
        command,
    } = &cli.command
    {
        let code = runner::run(
            &root,
            &cwd,
            &attribution.harness,
            attribution.model.as_deref().unwrap_or("unknown"),
            attribution.session.as_deref(),
            command,
        )?;
        return Ok(std::process::ExitCode::from(code.clamp(0, 255) as u8));
    }
    if let Action::Dashboard { port } = cli.command {
        {
            Ledger::open(root.clone())?;
        }
        dashboard::serve(root, port)?;
        return Ok(std::process::ExitCode::SUCCESS);
    }
    if matches!(&cli.command, Action::Blame { .. } | Action::Stat { .. }) {
        let snapshot = snapshot::ReadSnapshot::open(root.clone())?;
        report_action(&snapshot, cli.command, base)?;
        return Ok(std::process::ExitCode::SUCCESS);
    }
    let hook = match &cli.command {
        Action::Hook { .. } | Action::Record { stdin: Some(_), .. } => Some(hooks::read_claude()?),
        _ => None,
    };
    let ledger = Ledger::open(root.clone())?;
    match cli.command {
        Action::Init => {
            println!("Ledger ready. Run `aimark checkpoint` before your first AI edit.")
        }
        Action::Checkpoint { files } => {
            ledger.checkpoint(&paths(&ledger, &files, base)?)?;
        }
        Action::Record {
            attribution, files, ..
        } => {
            let files = if files.is_empty() {
                hook.as_ref()
                    .map(|h| h.files.iter().map(PathBuf::from).collect())
                    .unwrap_or(files)
            } else {
                files
            };
            let selected = paths(&ledger, &files, base)?;
            let model = attribution
                .model
                .or_else(|| hook.as_ref().and_then(|h| h.model.clone()))
                .unwrap_or_else(|| "unknown".into());
            let session = attribution
                .session
                .or_else(|| hook.as_ref().and_then(|h| h.session.clone()));
            if ledger.record(&attribution.harness, &model, session.as_deref(), &selected)? {
                eprintln!("aimark: recorded {}/{}", attribution.harness, model);
            }
        }
        Action::Hook { .. } => {
            let hook = hook.context("missing hook payload")?;
            let selected = paths(
                &ledger,
                &hook.files.iter().map(PathBuf::from).collect::<Vec<_>>(),
                &root,
            )?;
            ensure!(!selected.is_empty(), "hook has no eligible project files");
            match hook.event.as_str() {
                "PreToolUse" => {
                    ledger.checkpoint(&selected)?;
                }
                "PostToolUse" | "PostToolUseFailure" => {
                    ledger.record(
                        "claude-code",
                        hook.model.as_deref().unwrap_or("unknown"),
                        hook.session.as_deref(),
                        &selected,
                    )?;
                }
                _ => anyhow::bail!("unsupported hook event: {}", hook.event),
            }
        }
        Action::Dashboard { .. }
        | Action::Run { .. }
        | Action::Blame { .. }
        | Action::Stat { .. } => unreachable!(),
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn report_action(ledger: &snapshot::ReadSnapshot, action: Action, base: &Path) -> Result<()> {
    match action {
        Action::Blame { file, json } => {
            let file = ledger.path(&file, base)?;
            let lines = report::blame(ledger, &file)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&lines)?);
            } else {
                let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
                for line in lines {
                    let who = if line.pending {
                        "human (pending)"
                    } else {
                        &line.author
                    };
                    let row = format!("{:>5}  {:<24}  {}", line.line, who, line.text);
                    if color && line.ai {
                        println!("\x1b[32m{row}\x1b[0m");
                    } else {
                        println!("{row}");
                    }
                }
            }
        }
        Action::Stat { json } => {
            let stats = report::stats(ledger)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                for file in &stats.files {
                    println!(
                        "{:>6.1}%  {:>5}/{:<5}  {}",
                        file.pct,
                        file.ai,
                        file.total,
                        file.file.escape_debug()
                    );
                }
                println!(
                    "{:>6.1}%  {:>5}/{:<5}  TOTAL",
                    stats.pct, stats.ai, stats.total
                );
                if stats.pending > 0 {
                    eprintln!(
                        "{} pending lines count as human until recorded.",
                        stats.pending
                    );
                }
                if stats.skipped > 0 {
                    eprintln!(
                        "Skipped {} binary, symlink, or oversized files.",
                        stats.skipped
                    );
                }
            }
        }
        _ => unreachable!("only report commands reach report_action"),
    }
    Ok(())
}
