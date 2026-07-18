//! Collaboration convenience layer: derive and provision the collab substrate.
//!
//! `agent::collab`'s [`Plan`], [`Blackboard`], and [`Mailbox`] are the shared
//! coordination substrate multi-delegate work builds on, but the facade should
//! not force a user to hand-wire them (`docs/facade-api.md` §14). This module
//! turns the §14 default table into a small [`Collaboration`] config plus a
//! topology-driven derivation, and provisions the live, shared primitives an
//! [`Agent`](crate::facade::Agent) enables.
//!
//! # §14 default table
//!
//! | delegate topology | provisioned substrate |
//! |---|---|
//! | no delegate | none |
//! | one delegate, model-routed | mailbox optional, default **off** → none |
//! | multiple delegates | mailbox |
//! | dispatcher / verifier | plan + blackboard + mailbox |
//! | managed external agent | artifact store (additive) |
//!
//! An explicit [`Collaboration`] passed to
//! [`AgentBuilder::collaboration`](crate::facade::AgentBuilder::collaboration)
//! **replaces** the derived default in full, so a caller can turn any subset on:
//!
//! ```
//! use agent_lib::facade::Collaboration;
//!
//! let collab = Collaboration::new().plan().blackboard().mailbox().artifacts();
//! assert!(collab.plan_enabled() && collab.mailbox_enabled());
//! ```
//!
//! # What "enable" means here (and what it does not, per `PLAN.md` R8)
//!
//! Enabling a substrate provisions a real, live, shared primitive on the agent,
//! reachable through [`Agent::mailbox`](crate::facade::Agent::mailbox),
//! [`Agent::blackboard`](crate::facade::Agent::blackboard), and
//! [`Agent::plan`](crate::facade::Agent::plan). These are genuine
//! `agent::collab` objects — a caller, a delegate, or the managed-external
//! collab-event bridge can post to, read, and message through them.
//!
//! The provisioning layer deliberately does **not** advertise `agent::collab`
//! bridge tools to the supervising model or auto-route delegate coordination
//! through the primitives. §14's named mechanism for *populating* the mailbox /
//! blackboard / plan is the external-runtime collab-event bridge
//! (`CollabBridge`, M6-2): it normalizes a managed external delegate's
//! observations into writes against whichever primitives are provisioned. Every
//! §14 tier maps to a landed primitive, so no auto tier is silently skipped.
//!
//! # External collab-event bridge (§14 末段)
//!
//! A managed external delegate reports collaboration activity as provider-neutral
//! [`ExternalAgentEvent`] observations. The internal `CollabBridge` reflects the
//! three that §14 names into the shared substrate, keeping the coordination
//! observable and replayable across runtimes rather than reaching into any
//! runtime's private protocol (design §3.5):
//!
//! | external observation | bridged into |
//! |---|---|
//! | `send_message` → [`MessageSent`](ExternalAgentEvent::MessageSent) | [`Mailbox`] |
//! | `plan_update` → [`TaskUpdated`](ExternalAgentEvent::TaskUpdated) | [`Plan`] |
//! | `blackboard_post` → [`BlackboardPosted`](ExternalAgentEvent::BlackboardPosted) | [`Blackboard`] |
//!
//! `spawn_agent` is already bridged as a
//! [`NeedSubagent`](crate::agent::RequirementKind::NeedSubagent) requirement on
//! the external tool path (M3-3), so it is not re-reflected here. A disabled
//! substrate simply drops its events, and a status/label the [`Plan`] cannot
//! accept is skipped — the bridge is best-effort, matching the mailbox and
//! blackboard primitives it writes to.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::collab::TaskStatus;
use crate::agent::external::ExternalAgentEvent;
use crate::agent::{
    Blackboard, BlackboardSnapshot, Mailbox, MailboxSnapshot, Notification, Plan, PlanSnapshot,
};
use crate::facade::delegate::Delegation;
use crate::facade::ids::FacadeIds;

/// The set of collaboration substrates enabled on an agent (`docs/facade-api.md`
/// §14).
///
/// A `Collaboration` is a small data-only flag set. It is produced either by
/// topology derivation from a delegate topology or supplied explicitly through
/// [`AgentBuilder::collaboration`](crate::facade::AgentBuilder::collaboration);
/// an explicit value replaces the derived default in full. Build one fluently
/// from [`new`](Self::new):
///
/// ```
/// use agent_lib::facade::Collaboration;
///
/// let none = Collaboration::new();
/// assert!(!none.any());
///
/// let all = Collaboration::new().plan().blackboard().mailbox().artifacts();
/// assert!(all.plan_enabled() && all.blackboard_enabled());
/// assert!(all.mailbox_enabled() && all.artifacts_enabled());
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collaboration {
    /// Whether the shared [`Plan`] board is enabled.
    plan: bool,
    /// Whether the shared [`Blackboard`] is enabled.
    blackboard: bool,
    /// Whether the directed [`Mailbox`] is enabled.
    mailbox: bool,
    /// Whether the delegate artifact store is enabled.
    artifacts: bool,
}

impl Collaboration {
    /// An empty configuration with every substrate disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            plan: false,
            blackboard: false,
            mailbox: false,
            artifacts: false,
        }
    }

    /// Enables the shared [`Plan`] board.
    #[must_use]
    pub const fn plan(mut self) -> Self {
        self.plan = true;
        self
    }

    /// Enables the shared [`Blackboard`].
    #[must_use]
    pub const fn blackboard(mut self) -> Self {
        self.blackboard = true;
        self
    }

    /// Enables the directed [`Mailbox`].
    #[must_use]
    pub const fn mailbox(mut self) -> Self {
        self.mailbox = true;
        self
    }

    /// Enables the delegate artifact store.
    #[must_use]
    pub const fn artifacts(mut self) -> Self {
        self.artifacts = true;
        self
    }

    /// Reports whether the shared [`Plan`] board is enabled.
    #[must_use]
    pub const fn plan_enabled(&self) -> bool {
        self.plan
    }

    /// Reports whether the shared [`Blackboard`] is enabled.
    #[must_use]
    pub const fn blackboard_enabled(&self) -> bool {
        self.blackboard
    }

    /// Reports whether the directed [`Mailbox`] is enabled.
    #[must_use]
    pub const fn mailbox_enabled(&self) -> bool {
        self.mailbox
    }

    /// Reports whether the delegate artifact store is enabled.
    #[must_use]
    pub const fn artifacts_enabled(&self) -> bool {
        self.artifacts
    }

    /// Reports whether any substrate is enabled.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.plan || self.blackboard || self.mailbox || self.artifacts
    }
}

/// Derives the §14 default substrate set from a delegate topology.
///
/// `local_count` and `external_count` are the registered local subagent and
/// managed external delegate counts, and `delegation` is the resolved routing
/// mode. The rules mirror the §14 table exactly:
///
/// - no delegate at all → nothing;
/// - dispatcher-routed → plan + blackboard + mailbox (the escalation loop's
///   coordination substrate);
/// - otherwise two or more delegates → mailbox (a single model-routed delegate
///   stays off by default);
/// - any managed external delegate additionally enables the artifact store.
pub(crate) fn derive_default(
    delegation: &Delegation,
    local_count: usize,
    external_count: usize,
) -> Collaboration {
    let total = local_count + external_count;
    let mut config = Collaboration::new();
    if total == 0 {
        return config;
    }

    if delegation.is_dispatcher_routed() {
        config = config.plan().blackboard().mailbox();
    } else if total >= 2 {
        config = config.mailbox();
    }

    if external_count > 0 {
        config = config.artifacts();
    }

    config
}

/// Resolves the effective substrate set: an `explicit` config wins outright,
/// otherwise the topology-derived §14 default applies.
pub(crate) fn resolve(
    explicit: Option<Collaboration>,
    delegation: &Delegation,
    local_count: usize,
    external_count: usize,
) -> Collaboration {
    explicit.unwrap_or_else(|| derive_default(delegation, local_count, external_count))
}

/// The provisioned collaboration substrate carried by an
/// [`Agent`](crate::facade::Agent).
///
/// Holds the resolved [`Collaboration`] config plus the live, shared primitives
/// each enabled substrate provisions. The primitives are wrapped in `Arc` so an
/// agent, its delegates, and the (M6-2) external collab-event bridge share one
/// instance. A disabled substrate is `None`; the artifact store is a flag on
/// [`config`](Self::config) rather than a standalone object, because delegate
/// artifact references are already collected into
/// [`RunOutput::artifacts`](crate::facade::RunOutput) (M4).
#[derive(Debug)]
pub(crate) struct CollabState {
    /// The resolved substrate flags this state was provisioned from.
    pub(crate) config: Collaboration,
    /// The shared directed mailbox, when enabled.
    pub(crate) mailbox: Option<Arc<Mailbox>>,
    /// The shared blackboard, when enabled.
    pub(crate) blackboard: Option<Arc<Blackboard>>,
    /// The shared plan board, when enabled.
    pub(crate) plan: Option<Arc<Plan>>,
}

impl CollabState {
    /// Provisions the live primitives `config` enables, minting each primitive's
    /// identity from `ids`.
    pub(crate) fn provision(config: Collaboration, ids: &FacadeIds) -> Self {
        Self {
            config,
            mailbox: config.mailbox_enabled().then(|| Arc::new(Mailbox::new())),
            blackboard: config
                .blackboard_enabled()
                .then(|| Arc::new(Blackboard::new(ids.blackboard_id()))),
            plan: config
                .plan_enabled()
                .then(|| Arc::new(Plan::new(ids.plan_id()))),
        }
    }

    /// Restores the live primitives, treating a captured snapshot slice as the
    /// authoritative source of both *whether* a substrate exists and its
    /// *contents* (`docs/facade-api.md` §15.2).
    ///
    /// The conflict rule is **snapshot content wins; topology is only a provision
    /// hint for older snapshots**:
    ///
    /// - a captured slice always restores its substrate — even when the
    ///   topology-derived `config` would leave it disabled — rehydrating the
    ///   mailbox's inboxes and sequence cursor or the blackboard / plan's captured
    ///   identity and message / task history;
    /// - a *missing* slice falls back to `config`: an enabled-but-uncaptured
    ///   substrate (for example an older snapshot that predates collaboration
    ///   capture) provisions a fresh empty primitive, minting its identity from
    ///   `ids`, while a disabled-and-uncaptured substrate stays `None`.
    ///
    /// The effective [`config`](Self::config) is widened to cover every substrate
    /// the snapshot restored, so the advertised
    /// [`Collaboration`](crate::facade::Agent::collaboration) flags never disagree
    /// with the live primitives an accessor hands back.
    pub(crate) fn restore(
        config: Collaboration,
        ids: &FacadeIds,
        mailbox: Option<MailboxSnapshot>,
        blackboard: Option<BlackboardSnapshot>,
        plan: Option<PlanSnapshot>,
    ) -> Self {
        let mailbox = mailbox
            .map(|snapshot| Arc::new(Mailbox::from_snapshot(snapshot)))
            .or_else(|| config.mailbox_enabled().then(|| Arc::new(Mailbox::new())));
        let blackboard = blackboard
            .map(|snapshot| Arc::new(Blackboard::from_snapshot(snapshot)))
            .or_else(|| {
                config
                    .blackboard_enabled()
                    .then(|| Arc::new(Blackboard::new(ids.blackboard_id())))
            });
        let plan = plan
            .map(|snapshot| Arc::new(Plan::from_snapshot(snapshot)))
            .or_else(|| {
                config
                    .plan_enabled()
                    .then(|| Arc::new(Plan::new(ids.plan_id())))
            });

        // Widen the effective config so the advertised substrate flags stay
        // consistent with the primitives the snapshot restored (a snapshot slice
        // can enable a substrate the restored topology alone would not).
        let mut config = config;
        if mailbox.is_some() {
            config = config.mailbox();
        }
        if blackboard.is_some() {
            config = config.blackboard();
        }
        if plan.is_some() {
            config = config.plan();
        }

        Self {
            config,
            mailbox,
            blackboard,
            plan,
        }
    }
}

/// A shareable handle set that bridges a managed external delegate's collab
/// observations into the facade's provisioned collab substrate (§14 末段).
///
/// [`CollabState`] provisions the live shared primitives (M6-1); this bridge is
/// the mechanism that *populates* them from an external runtime. It holds only
/// cloned `Arc` handles to whichever substrates are enabled, so it is cheap to
/// clone into the per-run delegation handler and share with the child external
/// drive. A substrate the topology did not enable is `None`, and its events are
/// dropped.
///
/// The bridge is intentionally provider-neutral: it consumes agent-layer
/// [`ExternalAgentEvent`] observations (never a runtime's private message shape),
/// so the same collaboration protocol is observable and replayable across
/// runtimes (design §3.5).
#[derive(Clone, Debug, Default)]
pub(crate) struct CollabBridge {
    /// The shared directed mailbox, when enabled.
    mailbox: Option<Arc<Mailbox>>,
    /// The shared blackboard, when enabled.
    blackboard: Option<Arc<Blackboard>>,
    /// The shared plan board, when enabled.
    plan: Option<Arc<Plan>>,
}

impl CollabBridge {
    /// Builds a bridge over the primitives `state` provisioned.
    pub(crate) fn from_state(state: &CollabState) -> Self {
        Self {
            mailbox: state.mailbox.clone(),
            blackboard: state.blackboard.clone(),
            plan: state.plan.clone(),
        }
    }

    /// Reports whether any substrate is enabled, so a caller can skip scanning
    /// observations when nothing would be written.
    pub(crate) fn is_active(&self) -> bool {
        self.mailbox.is_some() || self.blackboard.is_some() || self.plan.is_some()
    }

    /// Reflects every collab observation in `notifications` into the enabled
    /// substrate, attributing each write to `from` (the delegate's name).
    ///
    /// Non-collab notifications and events whose substrate is disabled are
    /// ignored. A machine replays a decision point's observations exactly once
    /// (seq-deduped, design §5.5), so each event is absorbed a single time.
    pub(crate) fn absorb_notifications(&self, from: &str, notifications: &[Notification]) {
        if !self.is_active() {
            return;
        }
        for notification in notifications {
            if let Notification::ExternalAgent(event) = notification {
                self.absorb_event(from, event);
            }
        }
    }

    /// Reflects one external observation into the enabled substrate.
    ///
    /// - [`MessageSent`](ExternalAgentEvent::MessageSent) → [`Mailbox::send`];
    /// - [`TaskUpdated`](ExternalAgentEvent::TaskUpdated) → the [`Plan`]
    ///   (best-effort reconcile, see [`reflect_plan_update`]);
    /// - [`BlackboardPosted`](ExternalAgentEvent::BlackboardPosted) →
    ///   [`Blackboard::post`].
    ///
    /// Every other observation is not a collab event and is left alone.
    pub(crate) fn absorb_event(&self, from: &str, event: &ExternalAgentEvent) {
        match event {
            ExternalAgentEvent::MessageSent { to, summary } => {
                if let Some(mailbox) = &self.mailbox {
                    mailbox.send(from.to_owned(), to.to_string(), summary.clone());
                }
            }
            ExternalAgentEvent::TaskUpdated { task_id, status } => {
                if let Some(plan) = &self.plan {
                    reflect_plan_update(plan, from, task_id, status);
                }
            }
            ExternalAgentEvent::BlackboardPosted { channel, summary } => {
                if let Some(blackboard) = &self.blackboard {
                    blackboard.post(channel.clone(), from.to_owned(), summary.clone());
                }
            }
            _ => {}
        }
    }
}

/// Parses an external runtime's task-status label into a [`TaskStatus`], mapping
/// common provider synonyms onto the plan's canonical vocabulary.
///
/// The [`Plan`]'s own labels are accepted verbatim; beyond those a small synonym
/// set keeps the bridge provider-neutral (for example ACP reports `pending`
/// rather than `todo`, and runtimes commonly say `done`/`finished` for a
/// completed task). An unrecognized label yields `None`, so the reflection is a
/// best-effort no-op rather than a guess.
fn parse_task_status(label: &str) -> Option<TaskStatus> {
    match label.trim().to_ascii_lowercase().as_str() {
        "todo" | "pending" | "not_started" | "open" | "queued" => Some(TaskStatus::Todo),
        "in_progress" | "in-progress" | "active" | "running" | "started" => {
            Some(TaskStatus::InProgress)
        }
        "completed" | "complete" | "done" | "finished" => Some(TaskStatus::Completed),
        "blocked" | "waiting" | "stuck" => Some(TaskStatus::Blocked),
        "cancelled" | "canceled" | "abandoned" => Some(TaskStatus::Cancelled),
        _ => None,
    }
}

/// Reflects an external `plan_update` observation into the shared [`Plan`],
/// attributing the reporting delegate `owner`.
///
/// The [`Plan`] enforces ownership, legal transitions, and version CAS, but an
/// external observation is an authoritative *report*, not a model-issued claim.
/// This reconciles best-effort against the public API:
///
/// 1. add the task (with no dependencies) if the plan has never seen it;
/// 2. claim it for `owner` if it is unclaimed, so a status update is permitted;
/// 3. apply the reported [`TaskStatus`] when the transition is legal.
///
/// Every step is a no-op on failure (an illegal transition, a task owned by a
/// different agent, or an unparsable status label), so a noisy external plan
/// never corrupts the shared board.
fn reflect_plan_update(plan: &Plan, owner: &str, task_id: &str, status_label: &str) {
    let Some(status) = parse_task_status(status_label) else {
        return;
    };

    // Ensure the task exists before we try to move it.
    if !plan.snapshot().tasks.contains_key(task_id) {
        let _ = plan.add_task(task_id, Vec::<String>::new());
    }

    let snapshot = plan.snapshot();
    let Some(task) = snapshot.tasks.get(task_id) else {
        return;
    };
    if task.status == status {
        return;
    }

    // Establish ownership: an unclaimed task is claimed for `owner` (which moves
    // it to InProgress), so a subsequent `update_status` is authorized.
    let mut owned = task.owner.as_deref() == Some(owner);
    if !owned && task.owner.is_none() && status != TaskStatus::Todo {
        owned = plan.claim(task_id, owner, snapshot.version).is_ok();
    }
    if !owned {
        return;
    }

    // Re-read the version after any claim, then apply the target status when the
    // transition is legal (a no-op otherwise).
    let snapshot = plan.snapshot();
    if let Some(task) = snapshot.tasks.get(task_id)
        && task.status != status
        && task.status.can_transition_to(status)
    {
        let _ = plan.update_status(task_id, owner, status, snapshot.version);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CollabBridge, CollabState, Collaboration, derive_default, parse_task_status, resolve,
    };
    use crate::agent::collab::TaskStatus;
    use crate::agent::external::ExternalAgentEvent;
    use crate::agent::{AgentId, Mailbox, Notification};
    use crate::facade::delegate::Delegation;
    use crate::facade::ids::FacadeIds;

    #[test]
    fn no_delegate_enables_nothing() {
        let config = derive_default(&Delegation::model_routed(), 0, 0);
        assert!(!config.any(), "no delegate provisions no substrate");
    }

    #[test]
    fn single_model_routed_delegate_stays_off() {
        // §14: one delegate, model-routed → mailbox is optional, default off.
        let config = derive_default(&Delegation::model_routed(), 1, 0);
        assert_eq!(config, Collaboration::new());
        assert!(!config.mailbox_enabled());
    }

    #[test]
    fn multiple_delegates_auto_enable_only_mailbox() {
        // §14: multiple delegates → mailbox, and nothing heavier is silently
        // enabled (no plan / blackboard for a plain multi-delegate topology).
        let config = derive_default(&Delegation::model_routed(), 2, 0);
        assert!(
            config.mailbox_enabled(),
            "two delegates auto-enable mailbox"
        );
        assert!(!config.plan_enabled(), "plan is not silently enabled");
        assert!(
            !config.blackboard_enabled(),
            "blackboard is not silently enabled"
        );
        assert!(!config.artifacts_enabled());
    }

    #[test]
    fn dispatcher_enables_plan_blackboard_and_mailbox() {
        // §14: dispatcher / verifier → plan + blackboard + mailbox.
        let dispatcher = Delegation::dispatcher()
            .primary("cheap")
            .verify_with("checker")
            .escalate_to("strong");
        let config = derive_default(&dispatcher, 3, 0);
        assert!(config.plan_enabled());
        assert!(config.blackboard_enabled());
        assert!(config.mailbox_enabled());
        assert!(
            !config.artifacts_enabled(),
            "no external delegate → no artifacts"
        );
    }

    #[test]
    fn managed_external_delegate_enables_artifacts() {
        // §14: managed external agent → artifact store (additive). A lone
        // external delegate is one delegate, so mailbox stays off.
        let config = derive_default(&Delegation::model_routed(), 0, 1);
        assert!(config.artifacts_enabled());
        assert!(!config.mailbox_enabled());

        // A local + an external delegate is "multiple", so both mailbox and
        // artifacts turn on.
        let mixed = derive_default(&Delegation::model_routed(), 1, 1);
        assert!(mixed.mailbox_enabled());
        assert!(mixed.artifacts_enabled());
    }

    #[test]
    fn explicit_config_overrides_derived_default() {
        // A multi-delegate topology derives mailbox, but an explicit config
        // replaces the default in full.
        let explicit = Collaboration::new().plan();
        let resolved = resolve(Some(explicit), &Delegation::model_routed(), 3, 0);
        assert_eq!(resolved, explicit);
        assert!(resolved.plan_enabled());
        assert!(
            !resolved.mailbox_enabled(),
            "explicit config suppresses the derived mailbox"
        );
    }

    #[test]
    fn provision_creates_only_enabled_primitives() {
        let ids = FacadeIds::new();

        let none = CollabState::provision(Collaboration::new(), &ids);
        assert!(none.mailbox.is_none() && none.blackboard.is_none() && none.plan.is_none());

        let all = CollabState::provision(Collaboration::new().plan().blackboard().mailbox(), &ids);
        assert!(all.mailbox.is_some() && all.blackboard.is_some() && all.plan.is_some());
    }

    #[test]
    fn restore_without_snapshot_falls_back_to_topology_hint() {
        // With no captured slices, restore behaves like `provision`: the topology
        // config alone decides which empty primitives exist.
        let ids = FacadeIds::new();

        let none = CollabState::restore(Collaboration::new(), &ids, None, None, None);
        assert!(none.mailbox.is_none() && none.blackboard.is_none() && none.plan.is_none());

        let enabled = CollabState::restore(Collaboration::new().mailbox(), &ids, None, None, None);
        assert!(enabled.config.mailbox_enabled());
        assert!(
            enabled.mailbox.is_some(),
            "an enabled-but-uncaptured mailbox provisions an empty primitive"
        );
        assert!(enabled.blackboard.is_none() && enabled.plan.is_none());
    }

    #[test]
    fn restore_snapshot_content_overrides_disabled_config() {
        // Snapshot content wins even when the topology-derived config disables the
        // substrate: the mailbox restores and the effective config widens to match.
        let ids = FacadeIds::new();
        let source = Mailbox::new();
        source.send("reviewer", "researcher", "need sources");
        let captured = source.snapshot();

        let restored = CollabState::restore(Collaboration::new(), &ids, Some(captured), None, None);
        assert!(
            restored.config.mailbox_enabled(),
            "a captured mailbox widens the effective config"
        );
        let mailbox = restored.mailbox.expect("mailbox restored from snapshot");
        let inbox = mailbox.read_from("researcher", 0);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].text, "need sources");
        // The sequence cursor is authoritative: the next send continues it.
        assert_eq!(mailbox.send("planner", "researcher", "more"), 1);
        assert!(restored.blackboard.is_none() && restored.plan.is_none());
    }

    #[test]
    fn provisioned_mailbox_carries_directed_messages() {
        // The shared mailbox is a real primitive two delegates can message
        // through (the enable-mailbox behavior of §14).
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().mailbox(), &ids);
        let mailbox = state.mailbox.expect("mailbox provisioned");

        mailbox.send("worker-a", "worker-b", "please review task 1");
        mailbox.send("worker-b", "worker-a", "reviewed: looks good");

        let to_b = mailbox.inbox("worker-b");
        assert_eq!(to_b.len(), 1);
        assert_eq!(to_b[0].from, "worker-a");
        assert_eq!(to_b[0].text, "please review task 1");

        let to_a = mailbox.inbox("worker-a");
        assert_eq!(to_a.len(), 1);
        assert_eq!(to_a[0].text, "reviewed: looks good");
    }

    // ----- external collab-event bridge (M6-2) -----------------------------

    /// A recipient agent id the scripted `MessageSent` observation addresses.
    fn recipient_id() -> AgentId {
        AgentId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890b7").expect("agent id")
    }

    #[test]
    fn parse_task_status_accepts_canonical_and_synonyms() {
        assert_eq!(parse_task_status("todo"), Some(TaskStatus::Todo));
        assert_eq!(parse_task_status("pending"), Some(TaskStatus::Todo));
        assert_eq!(
            parse_task_status("in_progress"),
            Some(TaskStatus::InProgress)
        );
        assert_eq!(parse_task_status(" Running "), Some(TaskStatus::InProgress));
        assert_eq!(parse_task_status("completed"), Some(TaskStatus::Completed));
        assert_eq!(parse_task_status("DONE"), Some(TaskStatus::Completed));
        assert_eq!(parse_task_status("blocked"), Some(TaskStatus::Blocked));
        assert_eq!(parse_task_status("cancelled"), Some(TaskStatus::Cancelled));
        assert_eq!(parse_task_status("nonsense"), None);
    }

    #[test]
    fn bridge_over_no_substrate_is_inactive() {
        let ids = FacadeIds::new();
        let bridge = CollabBridge::from_state(&CollabState::provision(Collaboration::new(), &ids));
        assert!(!bridge.is_active());

        // Absorbing an event through an inactive bridge is a harmless no-op.
        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::BlackboardPosted {
                channel: "default".to_owned(),
                summary: "ignored".to_owned(),
            },
        );
    }

    #[test]
    fn bridge_routes_send_message_into_mailbox() {
        // §14: an external `send_message` observation lands in the shared mailbox.
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().mailbox(), &ids);
        let mailbox = state.mailbox.clone().expect("mailbox provisioned");
        let bridge = CollabBridge::from_state(&state);
        assert!(bridge.is_active());

        let to = recipient_id();
        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::MessageSent {
                to,
                summary: "handing off the parser fix".to_owned(),
            },
        );

        let inbox = mailbox.inbox(&to.to_string());
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "coder");
        assert_eq!(inbox[0].text, "handing off the parser fix");
    }

    #[test]
    fn bridge_routes_blackboard_post_into_blackboard() {
        // §14: an external `blackboard_post` observation appends to the shared
        // blackboard channel it names.
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().blackboard(), &ids);
        let blackboard = state.blackboard.clone().expect("blackboard provisioned");
        let bridge = CollabBridge::from_state(&state);

        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::BlackboardPosted {
                channel: "status".to_owned(),
                summary: "parser passes; moving to lints".to_owned(),
            },
        );

        let posts = blackboard.snapshot("status");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].sender, "coder");
        assert_eq!(posts[0].text, "parser passes; moving to lints");
    }

    #[test]
    fn bridge_reflects_plan_update_into_plan() {
        // §14: an external `plan_update` observation reconciles into the shared
        // plan — a fresh task is added, claimed, and driven to the reported
        // status, then a later update advances it.
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().plan(), &ids);
        let plan = state.plan.clone().expect("plan provisioned");
        let bridge = CollabBridge::from_state(&state);

        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::TaskUpdated {
                task_id: "parser".to_owned(),
                status: "in_progress".to_owned(),
            },
        );

        let snapshot = plan.snapshot();
        let task = snapshot.tasks.get("parser").expect("task added");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.owner.as_deref(), Some("coder"));

        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::TaskUpdated {
                task_id: "parser".to_owned(),
                status: "completed".to_owned(),
            },
        );
        assert_eq!(
            plan.snapshot().tasks.get("parser").expect("task").status,
            TaskStatus::Completed
        );
    }

    #[test]
    fn bridge_plan_update_reaching_completed_directly_claims_then_completes() {
        // A task reported straight as `completed` is added (Todo), claimed
        // (InProgress), then completed — the legal Todo→InProgress→Completed path.
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().plan(), &ids);
        let plan = state.plan.clone().expect("plan provisioned");
        let bridge = CollabBridge::from_state(&state);

        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::TaskUpdated {
                task_id: "lint".to_owned(),
                status: "done".to_owned(),
            },
        );
        assert_eq!(
            plan.snapshot().tasks.get("lint").expect("task").status,
            TaskStatus::Completed
        );
    }

    #[test]
    fn bridge_plan_update_with_unparsable_status_is_noop() {
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().plan(), &ids);
        let plan = state.plan.clone().expect("plan provisioned");
        let bridge = CollabBridge::from_state(&state);

        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::TaskUpdated {
                task_id: "mystery".to_owned(),
                status: "??".to_owned(),
            },
        );
        assert!(!plan.snapshot().tasks.contains_key("mystery"));
    }

    #[test]
    fn bridge_drops_events_for_disabled_substrates() {
        // Only the plan is enabled: a message and a blackboard post are dropped,
        // but the plan update still lands.
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().plan(), &ids);
        let plan = state.plan.clone().expect("plan provisioned");
        let bridge = CollabBridge::from_state(&state);

        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::MessageSent {
                to: recipient_id(),
                summary: "dropped".to_owned(),
            },
        );
        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::BlackboardPosted {
                channel: "default".to_owned(),
                summary: "dropped".to_owned(),
            },
        );
        bridge.absorb_event(
            "coder",
            &ExternalAgentEvent::TaskUpdated {
                task_id: "kept".to_owned(),
                status: "in_progress".to_owned(),
            },
        );

        assert!(bridge.mailbox.is_none() && bridge.blackboard.is_none());
        assert_eq!(
            plan.snapshot().tasks.get("kept").expect("task").status,
            TaskStatus::InProgress
        );
    }

    #[test]
    fn absorb_notifications_routes_only_external_collab_events() {
        // Non-external notifications are ignored; the collab observations wrapped
        // in `Notification::ExternalAgent` are routed to their substrate.
        let ids = FacadeIds::new();
        let state = CollabState::provision(Collaboration::new().mailbox().blackboard(), &ids);
        let mailbox = state.mailbox.clone().expect("mailbox provisioned");
        let blackboard = state.blackboard.clone().expect("blackboard provisioned");
        let bridge = CollabBridge::from_state(&state);

        let to = recipient_id();
        let notifications = vec![
            Notification::ExternalAgent(ExternalAgentEvent::SessionStarted { session_id: None }),
            Notification::ExternalAgent(ExternalAgentEvent::MessageSent {
                to,
                summary: "ping".to_owned(),
            }),
            Notification::ExternalAgent(ExternalAgentEvent::BlackboardPosted {
                channel: "default".to_owned(),
                summary: "posted".to_owned(),
            }),
        ];
        bridge.absorb_notifications("coder", &notifications);

        assert_eq!(mailbox.inbox(&to.to_string()).len(), 1);
        assert_eq!(blackboard.snapshot("default").len(), 1);
    }
}
