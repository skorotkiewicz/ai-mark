use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use tempfile::TempDir;

/// Pause actual Git blame processes until the test releases them.
pub struct GitGate {
    directory: TempDir,
    real_git: String,
    entered: PathBuf,
    release: PathBuf,
}

impl GitGate {
    pub fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let result = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(result.status.success());
        let real_git = String::from_utf8(result.stdout).unwrap().trim().to_owned();
        let entered = directory.path().join("entered");
        let release = directory.path().join("release");
        let script = directory.path().join("git");
        fs::write(&script, "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = blame ]; then\n    touch \"$AIMARK_TEST_GIT_ENTERED\"\n    while [ -d \"$AIMARK_TEST_GIT_DIRECTORY\" ] && [ ! -f \"$AIMARK_TEST_GIT_RELEASE\" ]; do sleep 0.02; done\n    break\n  fi\ndone\nexec \"$AIMARK_TEST_REAL_GIT\" \"$@\"\n").unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            directory,
            real_git,
            entered,
            release,
        }
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.directory.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("AIMARK_TEST_REAL_GIT", &self.real_git)
            .env("AIMARK_TEST_GIT_DIRECTORY", self.directory.path())
            .env("AIMARK_TEST_GIT_ENTERED", &self.entered)
            .env("AIMARK_TEST_GIT_RELEASE", &self.release);
    }

    pub fn wait_until_blocked(&self) {
        let start = Instant::now();
        while !self.entered.exists() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "report never reached its Git blame process"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn unblock(&self) {
        fs::write(&self.release, "continue").unwrap();
    }
}

impl Drop for GitGate {
    fn drop(&mut self) {
        let _ = fs::write(&self.release, "continue");
    }
}
