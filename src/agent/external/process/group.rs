//! Process-group lifecycle for managed external-agent subprocesses (H-EXT-2).
//!
//! A managed CLI session can spawn grandchildren of its own — builds, test
//! runners, dev servers launched through the CLI's shell tool. Killing only the
//! direct child orphans those grandchildren, which can keep writing to a
//! worktree the cleanup path is about to delete or reuse. On unix every managed
//! child is therefore spawned as a process-group leader
//! ([`configure_managed_command`]) and the force-close path ([`force_kill`])
//! signals the whole group: SIGTERM first, then SIGKILL after a short escalation
//! window.
//!
//! Windows has no POSIX process-group semantics, so the close path there keeps
//! tokio's direct-child `start_kill`; that platform difference is documented in
//! `docs/managed-external-agent.md` §16. The group guarantee covers the
//! cooperative `close` path only: a transport dropped without `close` still
//! relies on `kill_on_drop`, which reaps just the direct child.

use std::io;
use std::time::Duration;
use tokio::process::{Child, Command};

/// Grace window between the process-group SIGTERM and the SIGKILL escalation.
///
/// Bounded independently of the configured shutdown grace (which already elapsed
/// before [`force_kill`] runs) so a force-close stays fast; a process that ignores
/// SIGTERM only costs this window before SIGKILL lands.
#[cfg(unix)]
const SIGTERM_ESCALATION_GRACE: Duration = Duration::from_secs(2);

/// Configures a managed-agent child command to lead its own process group on
/// unix (`pgid == pid`), the precondition for group-wide signalling.
///
/// On non-unix platforms this is a no-op: Windows has no POSIX process-group
/// semantics, and the force-close path falls back to killing the direct child.
pub(crate) fn configure_managed_command(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(unix))]
    let _ = command;
}

/// Force-terminates `child` after its shutdown grace elapsed.
///
/// On unix the whole process group is signalled — SIGTERM first so well-behaved
/// grandchildren can exit on their own, then SIGKILL after
/// [`SIGTERM_ESCALATION_GRACE`] if the leader is still running — so CLI-spawned
/// grandchildren cannot outlive the session. If group signalling itself fails
/// (e.g. `EPERM`), the direct child is killed as a fallback so the leader never
/// survives a force-close. On other platforms this is tokio's direct-child
/// `start_kill`.
///
/// Returns `Ok(())` once the child has been terminated and reaped (the caller
/// classifies that as a forced kill); an `Err` means even the fallback kill
/// failed and the child may still be running.
pub(crate) async fn force_kill(child: &mut Child) -> io::Result<()> {
    force_kill_impl(child).await
}

/// Unix implementation: group SIGTERM, bounded wait, group SIGKILL escalation.
#[cfg(unix)]
async fn force_kill_impl(child: &mut Child) -> io::Result<()> {
    match signal_group(child, libc::SIGTERM) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            // The leader exited between the grace timeout and now, taking the
            // group with it; the wait below just reaps it.
        }
        Err(_error) => {
            // Group signal delivery failed (e.g. EPERM); at least kill the
            // leader so it never survives a force-close.
            child.start_kill()?;
            let _ = child.wait().await;
            return Ok(());
        }
    }
    match tokio::time::timeout(SIGTERM_ESCALATION_GRACE, child.wait()).await {
        Ok(_status) => Ok(()),
        Err(_elapsed) => {
            match signal_group(child, libc::SIGKILL) {
                Ok(()) => {}
                Err(_error) => child.start_kill()?,
            }
            let _ = child.wait().await;
            Ok(())
        }
    }
}

/// Non-unix implementation: no process groups, kill the direct child only.
#[cfg(not(unix))]
async fn force_kill_impl(child: &mut Child) -> io::Result<()> {
    child.start_kill()?;
    let _ = child.wait().await;
    Ok(())
}

/// Sends `signal` to the process group led by `child` (negative-pid `kill(2)`).
#[cfg(unix)]
fn signal_group(child: &Child, signal: libc::c_int) -> io::Result<()> {
    let pid = child.id().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "child process id unavailable")
    })?;
    // SAFETY: `kill` with a negative pid delivers `signal` to every process in
    // the group; no memory is accessed, and the pgid comes from a live child
    // handle that `configure_managed_command` made a group leader.
    let rc = unsafe { libc::kill(-(pid as i32), signal) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Asserts — with a short retry window — that no process remains in the group
/// led by `pgid`; shared by the per-adapter process-group close tests.
#[cfg(all(test, unix))]
pub(crate) async fn assert_process_group_reaped(pgid: i32) {
    for _ in 0..100 {
        // `kill(-pgid, 0)` performs error checking without delivering a signal;
        // ESRCH means no member of the group exists anymore. Grandchildren are
        // reaped by init asynchronously, hence the brief retry loop.
        let rc = unsafe { libc::kill(-pgid, 0) };
        if rc != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("process group {pgid} still has live members after force close");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Spawns a configured `sh -c <script>` child wired like the production
    /// transports (own process group, `kill_on_drop`).
    fn spawn_sh(script: &str) -> Child {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        configure_managed_command(&mut command);
        command.spawn().expect("spawn sh")
    }

    /// A configured command spawns the child as leader of its own process group
    /// (pgid == pid), the precondition for group-wide signalling.
    #[tokio::test]
    async fn configured_child_leads_its_own_process_group() {
        let mut child = spawn_sh("sleep 30");
        let pid = child.id().expect("child id") as i32;
        // Signal 0 probes group existence without delivering a signal.
        let rc = unsafe { libc::kill(-pid, 0) };
        assert_eq!(rc, 0, "process group led by the child must exist");
        child.kill().await.expect("kill child");
    }

    /// `force_kill` terminates the leader *and* its grandchildren: nothing in
    /// the group survives a force-close.
    #[tokio::test]
    async fn force_kill_reaps_the_whole_group() {
        let mut child = spawn_sh("sleep 300 & sleep 300");
        let pgid = child.id().expect("child id") as i32;
        force_kill(&mut child).await.expect("force kill");
        assert_process_group_reaped(pgid).await;
    }

    /// A leader that ignores SIGTERM (disposition preserved across `exec`) is
    /// escalated to SIGKILL within the escalation window.
    #[tokio::test]
    async fn force_kill_escalates_to_sigkill_when_sigterm_is_ignored() {
        let mut child = spawn_sh("trap '' TERM; sleep 300 & exec sleep 300");
        let pgid = child.id().expect("child id") as i32;
        force_kill(&mut child).await.expect("force kill");
        assert_process_group_reaped(pgid).await;
    }
}
