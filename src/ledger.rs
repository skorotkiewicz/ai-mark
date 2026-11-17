use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

pub const AI_MAIL: &str = "ai@aimark.local";

/// Ignore inherited Git state, including injected config from enclosing hooks.
pub fn clean_git() -> Command {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key
            .as_encoded_bytes()
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
        {
            cmd.env_remove(key);
        }
    }
    cmd
}

pub fn output(cmd: &mut Command) -> Result<Output> {
    let result = cmd
        .output()
        .context("could not run Git; install Git and put it on PATH")?;
    ensure!(
        result.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(result)
}

pub fn discover(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(root) = explicit {
        return root.canonicalize().context("invalid --root");
    }
    for ancestor in cwd.ancestors() {
        if ancestor.join(".aimark").exists() || ancestor.join(".git").exists() {
            return Ok(ancestor.to_owned());
        }
    }
    Ok(cwd.to_owned())
}

pub struct Ledger {
    pub root: PathBuf,
    // An OS lock is released even if a process crashes. The file stays in place.
    _lock: File,
}

fn ordinary_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "{} must be an ordinary directory",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ordinary_directory(path)?,
            Err(e) => return Err(e.into()),
        },
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn append_excludes(path: &Path, rules: &[&[u8]]) -> Result<()> {
    use std::io::Write;
    let existing = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    let mut addition = Vec::new();
    for rule in rules {
        if !existing
            .split(|b| *b == b'\n')
            .any(|line| line.strip_suffix(b"\r").unwrap_or(line) == *rule)
        {
            addition.push(b'\n');
            addition.extend_from_slice(rule);
            addition.push(b'\n');
        }
    }
    if !addition.is_empty() {
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?
            .write_all(&addition)?;
    }
    Ok(())
}

impl Ledger {
    pub fn open(root: PathBuf) -> Result<Self> {
        ensure!(root.is_dir(), "project root must be a directory");
        let dir = root.join(".aimark");
        ordinary_directory(&dir)?;
        let lock_path = dir.join("lock");
        if let Ok(m) = fs::symlink_metadata(&lock_path) {
            ensure!(
                m.is_file() && !m.file_type().is_symlink(),
                "invalid ledger lock file"
            );
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock()
            .context("could not lock the provenance ledger")?;
        let ledger = Self { root, _lock: lock };
        ledger.initialize()?;
        Ok(ledger)
    }

    pub fn git(&self) -> Command {
        let mut cmd = clean_git();
        // Keep filesystem paths canonical in Rust, but avoid Windows verbatim paths in Git arguments.
        // Git paths below are relative to the explicit project working directory.
        cmd.current_dir(&self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .arg("--literal-pathspecs")
            .arg("--git-dir")
            .arg(".aimark/repo")
            .arg("--work-tree")
            .arg(".")
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.quotepath=false",
                "-c",
                "core.autocrlf=false",
            ])
            .args(["-c", "core.hooksPath=.aimark/no-hooks"])
            .env("GIT_AUTHOR_NAME", "human")
            .env("GIT_AUTHOR_EMAIL", "human@aimark.local")
            .env("GIT_COMMITTER_NAME", "aimark")
            .env("GIT_COMMITTER_EMAIL", "ledger@aimark.local");
        cmd
    }

    fn initialize(&self) -> Result<()> {
        let repo = self.root.join(".aimark/repo");
        ordinary_directory(&self.root.join(".aimark/no-hooks"))?;
        if !repo.exists() {
            output(
                clean_git()
                    .current_dir(&self.root)
                    .args([
                        "-c",
                        "init.templateDir=",
                        "init",
                        "--bare",
                        "--quiet",
                        "--initial-branch=main",
                    ])
                    .arg(".aimark/repo"),
            )?;
            output(self.git().args(["config", "core.bare", "false"]))?;
        }
        ensure!(
            !fs::symlink_metadata(&repo)?.file_type().is_symlink(),
            "ledger repository must not be a symlink"
        );
        // Candidate filtering also excludes metadata if a worktree ignore file negates these rules.
        let exclude = repo.join("info/exclude");
        fs::create_dir_all(repo.join("info"))?;
        append_excludes(&exclude, &[b".git", b".aimark"])?;
        let head = self
            .git()
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .context("could not inspect ledger HEAD")?;
        if !head.status.success() {
            ensure!(
                head.status.code() == Some(1),
                "could not inspect ledger HEAD: {}",
                String::from_utf8_lossy(&head.stderr).trim()
            );
            output(self.git().args([
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "initialize provenance ledger",
            ]))?;
        }
        // git-path resolves ordinary repos, linked worktrees, and submodules.
        if self.root.join(".git").exists() {
            let result = clean_git()
                .current_dir(&self.root)
                .args(["rev-parse", "--git-path", "info/exclude"])
                .output()?;
            if result.status.success() {
                let name = String::from_utf8(result.stdout)?.trim().to_owned();
                let path = self.root.join(name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                append_excludes(&path, &[b"/.aimark/"])?;
            }
        }
        Ok(())
    }

    /// Resolve project-root aliases while rejecting symlinks inside the project and Git magic.
    pub fn path(&self, input: &Path, base: &Path) -> Result<String> {
        let absolute = if input.is_absolute() {
            input.to_owned()
        } else {
            base.join(input)
        };
        let mut normalized = PathBuf::new();
        for part in absolute.components() {
            match part {
                Component::ParentDir => {
                    ensure!(normalized.pop(), "path escapes project");
                }
                Component::CurDir => {}
                other => normalized.push(other.as_os_str()),
            }
        }
        let relative = match normalized.strip_prefix(&self.root) {
            Ok(relative) => relative.to_owned(),
            Err(_) => {
                // macOS temporary paths can reach the same root through an aliased ancestor.
                // Choose the outermost root alias so in-project symlinks remain in the suffix
                // and are rejected below. The suffix may name a new or deleted file.
                let root_alias = normalized
                    .ancestors()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .find(|ancestor| ancestor.canonicalize().is_ok_and(|path| path == self.root))
                    .context("path is outside the project")?;
                normalized.strip_prefix(root_alias)?.to_owned()
            }
        };
        ensure!(
            !relative.as_os_str().is_empty(),
            "select a file or subdirectory, not the project root"
        );
        let mut current = self.root.clone();
        for part in relative.components() {
            ensure!(
                part.as_os_str() != ".git" && part.as_os_str() != ".aimark",
                "internal metadata paths cannot be selected"
            );
            current.push(part.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(m) => ensure!(
                    !m.file_type().is_symlink(),
                    "symlink paths cannot be selected"
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let name = relative
            .to_str()
            .context("filenames must be UTF-8")?
            .replace(std::path::MAIN_SEPARATOR, "/");
        ensure!(
            !name.starts_with(':'),
            "Git pathspec syntax is not accepted"
        );
        Ok(name)
    }

    pub fn files(&self, paths: &[String]) -> Result<Vec<String>> {
        let result = output(
            self.git()
                .args([
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                    "--",
                ])
                .args(paths),
        )?;
        let mut files = result
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8(p.to_vec()).context("filenames must be UTF-8"))
            .collect::<Result<Vec<_>>>()?;
        files.sort();
        files.dedup();
        files.retain(|f| {
            !Path::new(f).components().any(|p| {
                p.as_os_str() == OsStr::new(".git") || p.as_os_str() == OsStr::new(".aimark")
            })
        });
        Ok(files)
    }

    pub fn checkpoint(&self, paths: &[String]) -> Result<bool> {
        self.commit("human", "human@aimark.local", "human checkpoint", paths)
    }

    pub fn record(
        &self,
        harness: &str,
        model: &str,
        session: Option<&str>,
        paths: &[String],
    ) -> Result<bool> {
        validate_metadata(harness)?;
        validate_metadata(model)?;
        if let Some(s) = session {
            validate_metadata(s)?;
        }
        let message = format!(
            "ai edit: {harness}\n\nAI-Harness: {harness}\nAI-Model: {model}\nAI-Session: {}",
            session.unwrap_or("-")
        );
        self.commit(&format!("{harness}/{model}"), AI_MAIL, &message, paths)
    }

    fn commit(&self, author: &str, email: &str, message: &str, paths: &[String]) -> Result<bool> {
        // Recover the index after an earlier failed snapshot without changing source files.
        output(self.git().args(["reset", "--mixed", "--quiet", "HEAD"]))?;
        let mut files = self.files(paths)?;
        let mut removed = Vec::new();
        files.retain(|file| {
            let mut ancestor = self.root.join(file);
            while ancestor.pop() && ancestor != self.root {
                if fs::symlink_metadata(&ancestor).is_ok_and(|m| !m.is_dir()) {
                    removed.push(file.clone());
                    return false;
                }
            }
            true
        });
        // Cached descendants become deletions when their directory is now a file or symlink.
        // Stage removals separately so ignored replacement files stay ignored.
        for file in removed {
            output(
                self.git()
                    .args(["update-index", "--force-remove", "--", &file]),
            )?;
        }
        if !files.is_empty() {
            use std::io::Write;
            use std::process::Stdio;
            let mut cmd = self.git();
            cmd.args(["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn()?;
            let mut input = child.stdin.take().context("Git stdin unavailable")?;
            for file in files {
                input.write_all(file.as_bytes())?;
                input.write_all(&[0])?;
            }
            drop(input);
            let result = child.wait_with_output()?;
            ensure!(
                result.status.success(),
                "Git add failed: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            );
        }
        let status = self
            .git()
            .args(["diff", "--cached", "--quiet", "--exit-code"])
            .status()?;
        match status.code() {
            Some(0) => Ok(false),
            Some(1) => {
                output(
                    self.git()
                        .env("GIT_AUTHOR_NAME", author)
                        .env("GIT_AUTHOR_EMAIL", email)
                        .args(["commit", "--quiet", "-m", message]),
                )?;
                Ok(true)
            }
            _ => bail!("Git could not inspect staged changes"),
        }
    }
}

pub fn validate_metadata(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= 512
            && !value
                .chars()
                .any(|c| c.is_control() || c == '<' || c == '>'),
        "harness, model, and session must be nonempty single-line values without angle brackets"
    );
    Ok(())
}
