//! Spawns cargo with a timeout, killing the whole process tree when it expires
//! or when the request is cancelled.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs `cargo <args>` inside `dir` and collects its output.
///
/// Fails if cargo cannot be spawned or if it runs longer than `timeout`, in
/// which case cargo and every process it started (rustc, test binaries…) are
/// killed.
pub async fn run_cargo(dir: &Path, args: &[String], timeout: Duration) -> Result<Output> {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.args(args)
        .current_dir(dir)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group, so the timeout can take down the whole tree.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().context("failed to spawn cargo")?;
    let mut guard = KillGuard(child.id());
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");

    // Read to EOF before reaping: a leftover grandchild holding the pipes open
    // then counts against the timeout, and the group id stays valid until the
    // wait below, so killing the group never hits an unrelated process.
    let run = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (o, e) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        o.context("failed to read cargo stdout")?;
        e.context("failed to read cargo stderr")?;
        let status = child.wait().await.context("failed to wait for cargo")?;
        anyhow::Ok(Output {
            status,
            stdout: out,
            stderr: err,
        })
    };

    match tokio::time::timeout(timeout, run).await {
        Ok(output) => {
            // cargo has been reaped, so its pid may be reused: never signal it now.
            guard.0 = None;
            output
        }
        Err(_) => {
            drop(guard);
            let _ = child.kill().await;
            bail!(
                "cargo timed out after {}s and was killed",
                timeout.as_secs()
            )
        }
    }
}

/// Kills the process group led by the given pid when dropped, which also
/// covers the request future being cancelled mid-run.
struct KillGuard(Option<u32>);

impl Drop for KillGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall; a stale or empty group just yields ESRCH.
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}
