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
    session: Option<&str>,
    command: &[OsString],
) -> Result<i32> {
    #[cfg(unix)]
    let signals = Signals::new()?;
    {
        Ledger::open(root.to_owned())?.checkpoint(&[])?;
    }
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(cwd)
        .spawn()
        .with_context(|| format!("could not start {}", command[0].to_string_lossy()))?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error),
            Ok(None) => {}
        }
        #[cfg(unix)]
        {
            use std::sync::atomic::Ordering;
            let signal = signals.pending.swap(0, Ordering::SeqCst) as i32;
            if signal != 0 {
                // SAFETY: the child has not been reaped, so its PID cannot have been reused.
                unsafe {
                    libc::kill(child.id() as libc::pid_t, signal);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    Ledger::open(root.to_owned())?.record(harness, model, session, &[])?;
    Ok(exit_code(
        status.context("could not wait for child process")?,
    ))
}

#[cfg(unix)]
struct Signals {
    pending: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    registrations: Vec<signal_hook::SigId>,
}
#[cfg(unix)]
impl Signals {
    fn new() -> Result<Self> {
        let mut result = Self {
            pending: Default::default(),
            registrations: Vec::new(),
        };
        for signal in [
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
        ] {
            result.registrations.push(signal_hook::flag::register_usize(
                signal,
                result.pending.clone(),
                signal as usize,
            )?);
        }
        Ok(result)
    }
}
#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
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
