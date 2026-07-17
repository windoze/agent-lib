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
//! What M6-1 deliberately does **not** do (to avoid promising unlanded
//! behavior): it neither advertises `agent::collab` bridge tools to the
//! supervising model nor auto-routes delegate coordination through the
//! primitives. §14's named mechanism for *populating* the mailbox / blackboard /
//! plan is the external-runtime collab-event bridge, which lands in M6-2; this
//! layer provisions the substrate that bridge writes into. Every §14 tier maps
//! to a landed primitive, so no auto tier is silently skipped.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::{Blackboard, Mailbox, Plan};
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
}

#[cfg(test)]
mod tests {
    use super::{CollabState, Collaboration, derive_default, resolve};
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
}
