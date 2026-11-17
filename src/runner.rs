use crate::ledger::Ledger;
use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, ExitStatus},
};

pub fn run(
    root: &Path,
    cwd: &Path,
    harness: &str,
    model: &str,
    command: &[OsString],
) -> Result<i32> {
    eprintln!("TRACE run start");
    #[cfg(unix)]
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ])?;
    {
        Ledger::open(root.to_owned())?.checkpoint(&[])?;
    }
    eprintln!("TRACE checkpoint done");
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(cwd)
        .spawn()
        .with_context(|| format!("could not start {}", command[0].to_string_lossy()))?;
    #[cfg(unix)]
    let (handle, thread) = {
        let pid = child.id() as libc::pid_t;
        let handle = signals.handle();
        let thread = std::thread::spawn(move || {
            for signal in signals.forever() {
                // SAFETY: pid is the child's process ID, signal is a registered Unix signal.
                unsafe {
                    libc::kill(pid, signal);
                }
            }
        });
        (handle, thread)
    };
    let status = child.wait();
    eprintln!("TRACE child done");
    #[cfg(unix)]
    {
        handle.close();
        let _ = thread.join();
    }
    eprintln!("TRACE signals done");
    Ledger::open(root.to_owned())?.record(harness, model, None, &[])?;
    eprintln!("TRACE record done");
    Ok(exit_code(
        status.context("could not wait for child process")?,
    ))
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(1)
    }
}
