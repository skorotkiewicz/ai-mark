use crate::ledger::{
    Ledger, acquire_lock, open_lock, output, project_path, repository_files, repository_git,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

/// A captured ledger revision with its own HEAD and no shared index or lasting lock.
pub struct ReadSnapshot {
    pub root: PathBuf,
    git_dir: PathBuf,
    files: Vec<String>,
    tracked: HashSet<String>,
    _directory: TempDir,
}

impl ReadSnapshot {
    pub fn open(root: PathBuf) -> Result<Self> {
        for _ in 0..2 {
            let lock = open_lock(&root)?;
            acquire_lock(&lock, true)?;
            let repo = root.join(".aimark/repo");
            if !repo.join("HEAD").exists() {
                drop(lock);
                drop(Ledger::open(root.clone())?);
                continue;
            }
            let metadata = fs::symlink_metadata(&repo)?;
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "ledger repository must be an ordinary directory"
            );
            let head = repository_git(&root, Path::new(".aimark/repo"))
                .args(["rev-parse", "--verify", "--quiet", "HEAD"])
                .output()?;
            if head.status.code() == Some(1) {
                // Recover an initialization interrupted before the first empty commit.
                drop(lock);
                drop(Ledger::open(root.clone())?);
                continue;
            }
            ensure!(
                head.status.success(),
                "could not inspect ledger HEAD: {}",
                String::from_utf8_lossy(&head.stderr).trim()
            );
            let files = repository_files(&root, Path::new(".aimark/repo"), &[])?;
            let directory = tempfile::Builder::new()
                .prefix("report-")
                .tempdir_in(root.join(".aimark"))?;
            fs::write(directory.path().join("HEAD"), &head.stdout)?;
            fs::write(directory.path().join("commondir"), "../repo\n")?;
            let git_dir = directory.path().strip_prefix(&root)?.to_owned();
            drop(lock);

            let mut snapshot = Self {
                root,
                git_dir,
                files,
                tracked: HashSet::new(),
                _directory: directory,
            };
            let tree = output(
                snapshot
                    .git()
                    .args(["ls-tree", "-r", "--name-only", "-z", "HEAD"]),
            )?;
            snapshot.tracked = tree
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| String::from_utf8(path.to_vec()).context("filenames must be UTF-8"))
                .collect::<Result<_>>()?;
            return Ok(snapshot);
        }
        bail!("could not initialize a ledger revision for reporting")
    }

    pub fn git(&self) -> Command {
        repository_git(&self.root, &self.git_dir)
    }

    pub fn path(&self, input: &Path, base: &Path) -> Result<String> {
        project_path(&self.root, input, base)
    }

    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn is_eligible(&self, file: &str) -> bool {
        self.files
            .binary_search_by(|path| path.as_str().cmp(file))
            .is_ok()
    }

    pub fn is_tracked(&self, file: &str) -> bool {
        self.tracked.contains(file)
    }
}
