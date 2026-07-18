//! Worktree isolation preparation and cleanup for managed external sessions.
//!
//! [`WorktreeIsolation`](super::WorktreeIsolation) is only *data*: it names how
//! strongly an external agent's edits should be isolated, but nothing in the
//! effect DTOs actually creates or tears down a working tree. This module adds
//! the layer that *executes* that policy (design §16):
//!
//! - [`WorktreeManager`] is the handler/scheduler-side hook that turns a
//!   requested [`WorktreeIsolation`] into a [`PreparedWorktree`] before a session
//!   starts and cleans it up afterward with the session's
//!   [`ExternalSessionShutdown`](super::ExternalSessionShutdown) disposition.
//! - [`GitWorktreeManager`] is the default implementation. `Shared` runs in the
//!   supplied checkout untouched, `PerAgentWorktree` gives each agent one stable
//!   linked git worktree reused across sessions, and `EphemeralGitWorktree`
//!   creates a fresh linked worktree per session and tears it down after a clean
//!   close.
//!
//! # Residual side effects (design §6.4, §16)
//!
//! A real runtime performs unrollbackable shell/edit/network actions, so *how* a
//! session closed decides whether its worktree is safe to reuse. Cleanup honors
//! the [`ExternalSessionShutdown`](super::ExternalSessionShutdown) disposition
//! the session registry recorded: a [`Graceful`](super::ExternalSessionShutdown::Graceful)
//! close of an ephemeral worktree removes it, while a
//! [`ForcedKill`](super::ExternalSessionShutdown::ForcedKill) or
//! [`Failed`](super::ExternalSessionShutdown::Failed) close *retains* the
//! worktree and marks the returned [`WorktreeCleanupOutcome`] as
//! [`residual_side_effects`](WorktreeCleanupOutcome::residual_side_effects) so a
//! scheduler never silently reuses a possibly-dirty tree. A shared or per-agent
//! worktree is never auto-removed, but the same residual marker is still raised
//! after a dirty close so the tree is not treated as clean.
//!
//! # Sans-io test boundary
//!
//! Git invocations go through the [`WorktreeGitExec`] hook so unit tests drive
//! the manager with a scripted executor and assert on the recorded commands and
//! disposition handling without a real repository (mirroring the probe-exec
//! idiom used by the runtime adapters). [`SystemGit`] is the production executor
//! that shells out to `git worktree`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use thiserror::Error;

use crate::agent::AgentId;
use crate::agent::spec::WorktreeRef;

use super::{ExternalSessionShutdown, WorktreeIsolation};

/// A worktree prepared for one external session, ready to run in.
///
/// [`WorktreeManager::prepare`] returns this after realizing the requested
/// [`WorktreeIsolation`]; the [`worktree`](Self::worktree) is the effective
/// path the session should run in (which differs from the requested base for the
/// per-agent and ephemeral policies). The value is handed back to
/// [`WorktreeManager::cleanup`] once the session closes so the manager can apply
/// the matching teardown policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedWorktree {
    agent_id: AgentId,
    isolation: WorktreeIsolation,
    worktree: WorktreeRef,
    base_repo: Option<WorktreeRef>,
    ephemeral: bool,
}

impl PreparedWorktree {
    /// Builds a prepared worktree from its parts.
    ///
    /// The built value carries no [`base_repo`](Self::base_repo) record, so a
    /// later cleanup falls back to using the worktree itself as the git `-C`
    /// directory (the pre-base-repo-record behavior). Prefer
    /// [`with_base_repo`](Self::with_base_repo) whenever the base repository is
    /// known — it always is for worktrees a [`WorktreeManager`] prepared.
    #[must_use]
    pub const fn new(
        agent_id: AgentId,
        isolation: WorktreeIsolation,
        worktree: WorktreeRef,
        ephemeral: bool,
    ) -> Self {
        Self {
            agent_id,
            isolation,
            worktree,
            base_repo: None,
            ephemeral,
        }
    }

    /// Records the base repository the worktree was linked from.
    ///
    /// Cleanup runs `git worktree` with the base repo as `-C` instead of the
    /// worktree itself, so teardown still works when the worktree's own `.git`
    /// link is damaged or the directory was moved (git can no longer discover
    /// the gitdir by walking up from the worktree).
    #[must_use]
    pub fn with_base_repo(mut self, base_repo: WorktreeRef) -> Self {
        self.base_repo = Some(base_repo);
        self
    }

    /// Returns the base repository recorded at preparation time, if any.
    #[must_use]
    pub const fn base_repo(&self) -> Option<&WorktreeRef> {
        self.base_repo.as_ref()
    }

    /// Returns the agent this worktree was prepared for.
    #[must_use]
    pub const fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    /// Returns the isolation policy this worktree realizes.
    #[must_use]
    pub const fn isolation(&self) -> WorktreeIsolation {
        self.isolation
    }

    /// Returns the effective worktree the session should run in.
    #[must_use]
    pub const fn worktree(&self) -> &WorktreeRef {
        &self.worktree
    }

    /// Returns `true` when this worktree is torn down after a clean close.
    ///
    /// Only [`EphemeralGitWorktree`](WorktreeIsolation::EphemeralGitWorktree)
    /// worktrees are ephemeral; shared and per-agent worktrees persist.
    #[must_use]
    pub const fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }
}

/// What cleaning up a [`PreparedWorktree`] did, so a scheduler can decide reuse.
///
/// This records whether the worktree was actually removed and whether the
/// session's close may have left unrollbackable side effects behind. A tree with
/// [`residual_side_effects`](Self::residual_side_effects) set must not be reused
/// as clean (design §6.4, §16).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeCleanupOutcome {
    isolation: WorktreeIsolation,
    worktree: WorktreeRef,
    removed: bool,
    residual_side_effects: bool,
}

impl WorktreeCleanupOutcome {
    /// Returns the isolation policy of the cleaned-up worktree.
    #[must_use]
    pub const fn isolation(&self) -> WorktreeIsolation {
        self.isolation
    }

    /// Returns the worktree that was cleaned up.
    #[must_use]
    pub const fn worktree(&self) -> &WorktreeRef {
        &self.worktree
    }

    /// Returns `true` when the worktree's backing directory was torn down.
    ///
    /// Only an ephemeral worktree closed gracefully is removed; a shared or
    /// per-agent worktree persists, and an ephemeral worktree closed with
    /// residual side effects is retained for inspection.
    #[must_use]
    pub const fn removed(&self) -> bool {
        self.removed
    }

    /// Returns `true` when the close may have left unrollbackable side effects.
    ///
    /// Mirrors
    /// [`ExternalSessionShutdown::leaves_residual_side_effects`](super::ExternalSessionShutdown::leaves_residual_side_effects)
    /// for the disposition cleanup ran with.
    #[must_use]
    pub const fn residual_side_effects(&self) -> bool {
        self.residual_side_effects
    }

    /// Returns `true` when the worktree is safe to reuse as clean.
    ///
    /// A worktree is reusable only when the close left no residual side effects.
    #[must_use]
    pub const fn safe_to_reuse(&self) -> bool {
        !self.residual_side_effects
    }
}

/// Why preparing or cleaning up an isolated worktree failed.
///
/// The manager attaches the offending [`WorktreeIsolation`] and worktree path to
/// the underlying git/filesystem failure so a caller can classify it without
/// parsing free-form text. The `detail` is a stable diagnostic and carries no
/// secret material.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WorktreeError {
    /// Realizing the requested isolation failed before the session could start.
    #[error("failed to prepare {isolation:?} worktree at {path}: {detail}")]
    Prepare {
        /// Isolation policy that could not be realized.
        isolation: WorktreeIsolation,
        /// Worktree path the failure concerned.
        path: String,
        /// Stable diagnostic text.
        detail: String,
    },
    /// Tearing the worktree down after the session closed failed.
    #[error("failed to clean up {isolation:?} worktree at {path}: {detail}")]
    Cleanup {
        /// Isolation policy whose teardown failed.
        isolation: WorktreeIsolation,
        /// Worktree path the failure concerned.
        path: String,
        /// Stable diagnostic text.
        detail: String,
    },
}

/// Executes the git operations a [`GitWorktreeManager`] needs.
///
/// Splitting these out keeps the manager's *policy* (which paths, when to remove)
/// separate from the *IO* (spawning `git`), so unit tests drive the policy with a
/// scripted executor and only the production [`SystemGit`] touches a real
/// repository. Both methods return a stable diagnostic string on failure, which
/// the manager lifts into a [`WorktreeError`].
#[async_trait]
pub trait WorktreeGitExec: Send + Sync {
    /// Adds a linked worktree at `worktree`, detached at `repo`'s current `HEAD`.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic string when `git worktree add` fails.
    async fn add_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), String>;

    /// Removes the linked worktree at `worktree`.
    ///
    /// `repo` must be the base repository the worktree was linked from (see
    /// [`PreparedWorktree::with_base_repo`]); the implementation may also rely
    /// on it to recover from a partially damaged or moved worktree that plain
    /// `git worktree remove` refuses to touch.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic string when `git worktree remove` fails.
    async fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), String>;
}

/// The production [`WorktreeGitExec`] that shells out to `git worktree`.
///
/// `add_worktree` runs `git -C <repo> worktree add --detach <path> HEAD`, giving
/// the session an isolated checkout of the base's current commit with no new
/// branch to collide on. `remove_worktree` runs
/// `git -C <repo> worktree remove --force <path>`, discarding the ephemeral tree
/// even when it carries uncommitted edits (the session's results are captured as
/// artifacts before a graceful close).
///
/// Removal also tolerates a worktree that was partially damaged (its `.git`
/// link deleted) or moved away: plain `git worktree remove` refuses such a tree
/// even from the base repo, so when the tree's `.git` link is gone the
/// implementation falls back to `git -C <repo> worktree prune` (which drops the
/// stale administrative entry) plus a direct deletion of any leftover
/// directory. A removal that fails for any other reason — the `.git` link is
/// intact, for example a locked tree — is reported as-is rather than forced.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemGit;

#[async_trait]
impl WorktreeGitExec for SystemGit {
    async fn add_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), String> {
        run_git(
            repo,
            &[
                "worktree".as_ref(),
                "add".as_ref(),
                "--detach".as_ref(),
                worktree.as_os_str(),
                "HEAD".as_ref(),
            ],
        )
        .await
    }

    async fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), String> {
        let remove = run_git(
            repo,
            &[
                "worktree".as_ref(),
                "remove".as_ref(),
                "--force".as_ref(),
                worktree.as_os_str(),
            ],
        )
        .await;
        let Err(remove_err) = remove else {
            return Ok(());
        };
        // Fall back only for a tree git can no longer validate: its `.git`
        // link is gone (deleted, or the whole directory was moved away). Any
        // other failure — the link is intact, e.g. a locked worktree — keeps
        // the original error instead of being force-deleted behind git's back.
        if worktree.join(".git").exists() {
            return Err(remove_err);
        }
        run_git(repo, &["worktree".as_ref(), "prune".as_ref()]).await?;
        match std::fs::remove_dir_all(worktree) {
            Ok(()) => Ok(()),
            // The tree was moved away: prune already dropped the admin entry.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!(
                "{remove_err}; failed to remove leftover worktree dir {}: {err}",
                worktree.display()
            )),
        }
    }
}

/// Runs `git -C <repo> <args...>`, returning a diagnostic string on failure.
async fn run_git(repo: &Path, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(repo);
    command.args(args);
    let output = command
        .output()
        .await
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "git exited with {}: {}",
        output.status,
        stderr.trim()
    ))
}

/// Prepares and cleans up worktrees for managed external sessions.
///
/// A handler/scheduler calls [`prepare`](Self::prepare) before starting a session
/// and [`cleanup`](Self::cleanup) after the session closes, feeding in the
/// [`ExternalSessionShutdown`](super::ExternalSessionShutdown) disposition the
/// session registry reported so residual-side-effect policy is applied uniformly
/// (design §6.4, §16). It is object-safe so a host can hold one as
/// `Arc<dyn WorktreeManager>` across runtime kinds.
#[async_trait]
pub trait WorktreeManager: Send + Sync {
    /// Realizes `isolation` for `agent_id` starting from the `base` worktree.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeError::Prepare`] when the worktree could not be created.
    async fn prepare(
        &self,
        agent_id: AgentId,
        base: &WorktreeRef,
        isolation: WorktreeIsolation,
    ) -> Result<PreparedWorktree, WorktreeError>;

    /// Tears down `prepared` according to `disposition`.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeError::Cleanup`] when an ephemeral worktree that should
    /// be removed could not be torn down.
    async fn cleanup(
        &self,
        prepared: PreparedWorktree,
        disposition: ExternalSessionShutdown,
    ) -> Result<WorktreeCleanupOutcome, WorktreeError>;
}

/// The default [`WorktreeManager`], backed by linked git worktrees.
///
/// Prepared worktrees for the per-agent and ephemeral policies live under a
/// single [`root`](Self::with_root) directory (by default
/// `std::env::temp_dir()/agent-lib-worktrees`), kept outside the base checkout so
/// git worktrees are not nested inside the repository they branch from. The git
/// operations are delegated to a [`WorktreeGitExec`] so the placement/teardown
/// *policy* here stays testable without a real repository.
///
/// Ephemeral worktree names are made unique with a per-manager monotonic counter
/// rather than a random or clock-based token, so a live scheduler never collides
/// two sessions and the crate keeps its "callers own all nondeterminism" stance
/// (see [`AgentId`](crate::agent::AgentId)); a retained (dirty-closed) tree from a
/// previous run is skipped by an existence check.
#[derive(Debug)]
pub struct GitWorktreeManager<G = SystemGit> {
    git: G,
    root: PathBuf,
    next_ephemeral: AtomicU64,
}

impl GitWorktreeManager<SystemGit> {
    /// Creates a manager that shells out to the system `git`.
    ///
    /// Per-agent and ephemeral worktrees are placed under
    /// `std::env::temp_dir()/agent-lib-worktrees`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_git_exec(SystemGit)
    }
}

impl Default for GitWorktreeManager<SystemGit> {
    fn default() -> Self {
        Self::new()
    }
}

impl<G> GitWorktreeManager<G> {
    /// Creates a manager with an explicit git executor and the default root.
    #[must_use]
    pub fn with_git_exec(git: G) -> Self {
        Self {
            git,
            root: std::env::temp_dir().join("agent-lib-worktrees"),
            next_ephemeral: AtomicU64::new(0),
        }
    }

    /// Overrides the root directory prepared worktrees are placed under.
    ///
    /// Hosts (and tests) use this to keep managed worktrees inside a chosen
    /// scratch area rather than the system temp directory.
    #[must_use]
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// Returns the root directory prepared worktrees are placed under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Deterministic per-agent worktree path: `<root>/agent-<agent_id>`.
    fn per_agent_path(&self, agent_id: AgentId) -> PathBuf {
        self.root.join(format!("agent-{agent_id}"))
    }

    /// Per-session ephemeral worktree path: `<root>/ephemeral/<agent_id>-<n>`.
    ///
    /// `n` is drawn from a per-manager counter and advanced past any path that
    /// already exists (a retained, dirty-closed tree), so each live session gets
    /// a fresh directory without relying on random or clock-based tokens.
    fn ephemeral_path(&self, agent_id: AgentId) -> PathBuf {
        let dir = self.root.join("ephemeral");
        loop {
            let n = self.next_ephemeral.fetch_add(1, Ordering::Relaxed);
            let candidate = dir.join(format!("{agent_id}-{n}"));
            if !candidate.exists() {
                return candidate;
            }
        }
    }
}

#[async_trait]
impl<G> WorktreeManager for GitWorktreeManager<G>
where
    G: WorktreeGitExec,
{
    async fn prepare(
        &self,
        agent_id: AgentId,
        base: &WorktreeRef,
        isolation: WorktreeIsolation,
    ) -> Result<PreparedWorktree, WorktreeError> {
        match isolation {
            WorktreeIsolation::Shared => {
                Ok(
                    PreparedWorktree::new(agent_id, isolation, base.clone(), false)
                        .with_base_repo(base.clone()),
                )
            }
            WorktreeIsolation::PerAgentWorktree => {
                let path = self.per_agent_path(agent_id);
                // A per-agent worktree is stable and reused across sessions, so
                // preparing it is idempotent: only add it the first time.
                if !path.exists() {
                    self.git
                        .add_worktree(base.path(), &path)
                        .await
                        .map_err(|detail| WorktreeError::Prepare {
                            isolation,
                            path: path.display().to_string(),
                            detail,
                        })?;
                }
                Ok(
                    PreparedWorktree::new(agent_id, isolation, WorktreeRef::new(path), false)
                        .with_base_repo(base.clone()),
                )
            }
            WorktreeIsolation::EphemeralGitWorktree => {
                let path = self.ephemeral_path(agent_id);
                self.git
                    .add_worktree(base.path(), &path)
                    .await
                    .map_err(|detail| WorktreeError::Prepare {
                        isolation,
                        path: path.display().to_string(),
                        detail,
                    })?;
                Ok(
                    PreparedWorktree::new(agent_id, isolation, WorktreeRef::new(path), true)
                        .with_base_repo(base.clone()),
                )
            }
        }
    }

    async fn cleanup(
        &self,
        prepared: PreparedWorktree,
        disposition: ExternalSessionShutdown,
    ) -> Result<WorktreeCleanupOutcome, WorktreeError> {
        let residual = disposition.leaves_residual_side_effects();
        let isolation = prepared.isolation;
        let worktree = prepared.worktree.clone();

        // Only an ephemeral worktree closed cleanly is torn down. A dirty close
        // (forced kill / failed) retains even an ephemeral tree for inspection,
        // and shared / per-agent worktrees are never auto-removed.
        let removed = if prepared.ephemeral && !residual {
            // Run git from the recorded base repo, not the worktree itself: a
            // partially damaged or moved worktree no longer lets git discover
            // the gitdir by walking up from it. A `PreparedWorktree` built by
            // hand without a base repo record falls back to the old behavior.
            let repo = prepared
                .base_repo
                .as_ref()
                .map_or_else(|| prepared.worktree.path(), WorktreeRef::path);
            self.git
                .remove_worktree(repo, prepared.worktree.path())
                .await
                .map_err(|detail| WorktreeError::Cleanup {
                    isolation,
                    path: prepared.worktree.path().display().to_string(),
                    detail,
                })?;
            true
        } else {
            false
        };

        Ok(WorktreeCleanupOutcome {
            isolation,
            worktree,
            removed,
            residual_side_effects: residual,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    fn agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f0"
            .parse()
            .expect("agent id")
    }

    fn other_agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f5"
            .parse()
            .expect("agent id")
    }

    /// Records the git commands issued and simulates the filesystem side of
    /// `git worktree add` by creating the directory, so the manager's
    /// existence-based idempotency can be exercised without a real repository.
    #[derive(Default)]
    struct ScriptedGit {
        added: Mutex<Vec<PathBuf>>,
        removed: Mutex<Vec<PathBuf>>,
        removed_repos: Mutex<Vec<PathBuf>>,
        fail_add: bool,
        fail_remove: bool,
    }

    #[async_trait]
    impl WorktreeGitExec for ScriptedGit {
        async fn add_worktree(&self, _repo: &Path, worktree: &Path) -> Result<(), String> {
            if self.fail_add {
                return Err("simulated add failure".to_owned());
            }
            std::fs::create_dir_all(worktree).expect("create scripted worktree dir");
            self.added.lock().unwrap().push(worktree.to_path_buf());
            Ok(())
        }

        async fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), String> {
            if self.fail_remove {
                return Err("simulated remove failure".to_owned());
            }
            let _ = std::fs::remove_dir_all(worktree);
            self.removed.lock().unwrap().push(worktree.to_path_buf());
            self.removed_repos.lock().unwrap().push(repo.to_path_buf());
            Ok(())
        }
    }

    /// A unique scratch root per test, torn down on drop.
    struct ScratchRoot {
        path: PathBuf,
    }

    impl ScratchRoot {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("agent-lib-wt-test-{}-{n}", std::process::id()));
            Self { path }
        }
    }

    impl Drop for ScratchRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn base() -> WorktreeRef {
        WorktreeRef::new("/repo/agent-lib")
    }

    #[tokio::test]
    async fn external_worktree_shared_prepare_runs_in_base_without_git() {
        let scratch = ScratchRoot::new();
        let git = ScriptedGit::default();
        let manager = GitWorktreeManager::with_git_exec(git).with_root(&scratch.path);

        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::Shared)
            .await
            .expect("prepare shared");

        assert_eq!(prepared.worktree(), &base());
        assert_eq!(prepared.isolation(), WorktreeIsolation::Shared);
        assert!(!prepared.is_ephemeral());
        // Shared isolation touches no git.
        assert!(manager.git.added.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_worktree_shared_cleanup_never_removes_but_flags_residual() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);
        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::Shared)
            .await
            .expect("prepare");

        let graceful = manager
            .cleanup(prepared.clone(), ExternalSessionShutdown::Graceful)
            .await
            .expect("graceful cleanup");
        assert!(!graceful.removed());
        assert!(!graceful.residual_side_effects());
        assert!(graceful.safe_to_reuse());

        let forced = manager
            .cleanup(prepared, ExternalSessionShutdown::ForcedKill)
            .await
            .expect("forced cleanup");
        // A shared worktree is never removed, but a dirty close still marks it.
        assert!(!forced.removed());
        assert!(forced.residual_side_effects());
        assert!(!forced.safe_to_reuse());
        assert!(manager.git.removed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_worktree_per_agent_is_added_once_and_reused() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);

        let first = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::PerAgentWorktree)
            .await
            .expect("first prepare");
        let second = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::PerAgentWorktree)
            .await
            .expect("second prepare");

        // Deterministic, stable path reused across sessions.
        assert_eq!(first.worktree(), second.worktree());
        assert!(!first.is_ephemeral());
        assert_eq!(
            first.worktree().path(),
            scratch.path.join(format!("agent-{}", agent_id()))
        );
        // Added exactly once despite two prepares (idempotent reuse).
        assert_eq!(manager.git.added.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn external_worktree_per_agent_paths_differ_between_agents() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);

        let one = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::PerAgentWorktree)
            .await
            .expect("agent one");
        let two = manager
            .prepare(
                other_agent_id(),
                &base(),
                WorktreeIsolation::PerAgentWorktree,
            )
            .await
            .expect("agent two");

        assert_ne!(one.worktree(), two.worktree());
        assert_eq!(manager.git.added.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn external_worktree_per_agent_cleanup_persists_and_flags_dirty_close() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);
        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::PerAgentWorktree)
            .await
            .expect("prepare");

        let failed = manager
            .cleanup(prepared, ExternalSessionShutdown::Failed)
            .await
            .expect("failed cleanup");
        // Persistent worktree: never removed, but a failed close is not clean.
        assert!(!failed.removed());
        assert!(failed.residual_side_effects());
        assert!(!failed.safe_to_reuse());
        assert!(manager.git.removed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_worktree_ephemeral_prepare_adds_unique_worktrees() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);

        let first = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect("first ephemeral");
        let second = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect("second ephemeral");

        assert!(first.is_ephemeral());
        assert!(second.is_ephemeral());
        // Each session gets a fresh, distinct worktree.
        assert_ne!(first.worktree(), second.worktree());
        let added: HashSet<PathBuf> = manager.git.added.lock().unwrap().iter().cloned().collect();
        assert_eq!(added.len(), 2);
    }

    #[tokio::test]
    async fn external_worktree_ephemeral_graceful_close_is_removed() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);
        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect("prepare");
        let path = prepared.worktree().path().to_path_buf();

        let outcome = manager
            .cleanup(prepared, ExternalSessionShutdown::Graceful)
            .await
            .expect("graceful cleanup");

        assert!(outcome.removed());
        assert!(!outcome.residual_side_effects());
        assert!(outcome.safe_to_reuse());
        assert_eq!(manager.git.removed.lock().unwrap().as_slice(), &[path]);
    }

    #[tokio::test]
    async fn external_worktree_ephemeral_forced_kill_is_retained_and_marked() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);
        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect("prepare");

        let outcome = manager
            .cleanup(prepared, ExternalSessionShutdown::ForcedKill)
            .await
            .expect("forced cleanup");

        // A forced kill must never remove or mark the ephemeral tree clean.
        assert!(!outcome.removed());
        assert!(outcome.residual_side_effects());
        assert!(!outcome.safe_to_reuse());
        assert!(manager.git.removed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_worktree_ephemeral_failed_close_is_retained_and_marked() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);
        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect("prepare");

        let outcome = manager
            .cleanup(prepared, ExternalSessionShutdown::Failed)
            .await
            .expect("failed cleanup");

        assert!(!outcome.removed());
        assert!(outcome.residual_side_effects());
        assert!(manager.git.removed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_worktree_prepare_surfaces_git_add_failure() {
        let scratch = ScratchRoot::new();
        let git = ScriptedGit {
            fail_add: true,
            ..ScriptedGit::default()
        };
        let manager = GitWorktreeManager::with_git_exec(git).with_root(&scratch.path);

        let err = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect_err("add failure surfaces");
        match err {
            WorktreeError::Prepare {
                isolation, detail, ..
            } => {
                assert_eq!(isolation, WorktreeIsolation::EphemeralGitWorktree);
                assert!(detail.contains("simulated add failure"));
            }
            other => panic!("expected Prepare error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn external_worktree_cleanup_surfaces_git_remove_failure() {
        let scratch = ScratchRoot::new();
        let git = ScriptedGit {
            fail_remove: true,
            ..ScriptedGit::default()
        };
        let manager = GitWorktreeManager::with_git_exec(git).with_root(&scratch.path);
        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect("prepare");

        let err = manager
            .cleanup(prepared, ExternalSessionShutdown::Graceful)
            .await
            .expect_err("remove failure surfaces");
        match err {
            WorktreeError::Cleanup { isolation, .. } => {
                assert_eq!(isolation, WorktreeIsolation::EphemeralGitWorktree);
            }
            other => panic!("expected Cleanup error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn external_worktree_cleanup_disposition_drives_residual_marker() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);

        // The disposition a session registry cleanup reports is exactly what the
        // worktree cleanup consumes; each maps to the documented reuse policy.
        for (disposition, expect_residual) in [
            (ExternalSessionShutdown::Graceful, false),
            (ExternalSessionShutdown::ForcedKill, true),
            (ExternalSessionShutdown::Failed, true),
        ] {
            let prepared = manager
                .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
                .await
                .expect("prepare");
            let outcome = manager
                .cleanup(prepared, disposition)
                .await
                .expect("cleanup");
            assert_eq!(outcome.residual_side_effects(), expect_residual);
            assert_eq!(
                outcome.residual_side_effects(),
                disposition.leaves_residual_side_effects()
            );
            // A clean close removes the ephemeral tree; a dirty one retains it.
            assert_eq!(outcome.removed(), !expect_residual);
        }
    }

    #[tokio::test]
    async fn external_worktree_cleanup_runs_git_from_recorded_base_repo() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);
        let prepared = manager
            .prepare(agent_id(), &base(), WorktreeIsolation::EphemeralGitWorktree)
            .await
            .expect("prepare");
        assert_eq!(prepared.base_repo(), Some(&base()));

        manager
            .cleanup(prepared, ExternalSessionShutdown::Graceful)
            .await
            .expect("cleanup");

        // `-C` is the recorded base repo, not the worktree itself.
        assert_eq!(
            manager.git.removed_repos.lock().unwrap().as_slice(),
            &[base().path().to_path_buf()]
        );
    }

    #[tokio::test]
    async fn external_worktree_cleanup_without_base_repo_falls_back_to_worktree() {
        let scratch = ScratchRoot::new();
        let manager =
            GitWorktreeManager::with_git_exec(ScriptedGit::default()).with_root(&scratch.path);
        // A hand-built `PreparedWorktree` carries no base repo record; cleanup
        // keeps the pre-record behavior of using the worktree itself as `-C`.
        let worktree = WorktreeRef::new(scratch.path.join("hand-built"));
        let prepared = PreparedWorktree::new(
            agent_id(),
            WorktreeIsolation::EphemeralGitWorktree,
            worktree.clone(),
            true,
        );
        assert_eq!(prepared.base_repo(), None);

        manager
            .cleanup(prepared, ExternalSessionShutdown::Graceful)
            .await
            .expect("cleanup");

        assert_eq!(
            manager.git.removed_repos.lock().unwrap().as_slice(),
            &[worktree.path().to_path_buf()]
        );
    }

    /// Returns `true` when a `git` binary is on `PATH`; real-git tests skip
    /// cleanly otherwise (they never touch the network or user config).
    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// Runs `git -C <repo> <args...>` synchronously, panicking on failure.
    fn git_in(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Initializes a real git repository with one commit under `scratch`.
    fn init_real_repo(scratch: &Path) -> PathBuf {
        let repo = scratch.join("base");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        git_in(&repo, &["init", "-q"]);
        git_in(
            &repo,
            &["config", "user.email", "agent-lib@example.invalid"],
        );
        git_in(&repo, &["config", "user.name", "agent-lib tests"]);
        std::fs::write(repo.join("file"), "contents").expect("seed file");
        git_in(&repo, &["add", "file"]);
        git_in(&repo, &["commit", "-qm", "init"]);
        repo
    }

    /// Asserts the base repo no longer has an administrative entry for `path`.
    fn assert_worktree_deregistered(repo: &Path, path: &Path) {
        let list = git_in(repo, &["worktree", "list", "--porcelain"]);
        assert!(
            !list
                .lines()
                .any(|line| line == format!("worktree {}", path.display())),
            "worktree {} still registered:\n{list}",
            path.display()
        );
    }

    #[tokio::test]
    async fn external_worktree_cleanup_survives_corrupt_git_link_via_base_repo() {
        if !git_available() {
            eprintln!("skipping: git binary not available");
            return;
        }
        let scratch = ScratchRoot::new();
        let repo = init_real_repo(&scratch.path);
        let manager = GitWorktreeManager::new().with_root(scratch.path.join("wt"));
        let prepared = manager
            .prepare(
                agent_id(),
                &WorktreeRef::new(&repo),
                WorktreeIsolation::EphemeralGitWorktree,
            )
            .await
            .expect("prepare");
        let worktree = prepared.worktree().path().to_path_buf();

        // Simulate partial damage: the worktree's `.git` link is gone, so git
        // can neither discover the gitdir from the worktree nor validate it for
        // a plain `git worktree remove` (even from the base repo).
        std::fs::remove_file(worktree.join(".git")).expect("corrupt git link");

        let outcome = manager
            .cleanup(prepared, ExternalSessionShutdown::Graceful)
            .await
            .expect("cleanup survives corrupt git link");
        assert!(outcome.removed());
        assert!(!worktree.exists(), "leftover directory removed");
        assert_worktree_deregistered(&repo, &worktree);
    }

    #[tokio::test]
    async fn external_worktree_cleanup_survives_moved_worktree_via_base_repo() {
        if !git_available() {
            eprintln!("skipping: git binary not available");
            return;
        }
        let scratch = ScratchRoot::new();
        let repo = init_real_repo(&scratch.path);
        let manager = GitWorktreeManager::new().with_root(scratch.path.join("wt"));
        let prepared = manager
            .prepare(
                agent_id(),
                &WorktreeRef::new(&repo),
                WorktreeIsolation::EphemeralGitWorktree,
            )
            .await
            .expect("prepare");
        let worktree = prepared.worktree().path().to_path_buf();

        // Simulate the whole tree being moved away before cleanup.
        let moved = scratch.path.join("moved-away");
        std::fs::rename(&worktree, &moved).expect("move worktree");

        let outcome = manager
            .cleanup(prepared, ExternalSessionShutdown::Graceful)
            .await
            .expect("cleanup survives moved worktree");
        assert!(outcome.removed());
        assert_worktree_deregistered(&repo, &worktree);
    }
}
