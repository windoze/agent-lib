//! Worker capability / cost profiles for the mixed-agent scheduler.
//!
//! A *worker* in the mixed-agent session (design §8) is any machine that can be
//! dispatched to handle a task — an internal cheap [`DefaultAgentMachine`], a
//! low-cost DeepSeek agent, or an [`ExternalAgentMachine`](super::ExternalAgentMachine)
//! backing Claude Code / Codex / OpenCode. Different workers are *not*
//! homogeneous: they differ in capability, price band, and how far the scheduler
//! should escalate when they fail.
//!
//! This module captures that heterogeneity as data so a later dispatcher (design
//! §9) can make cost-aware / capability-aware routing decisions:
//!
//! - [`WorkerProfile`] is the full, data-only description of one worker: the
//!   [`Capability`] tags it advertises, its [`CostTier`] price band, and the
//!   [`EscalationRules`] that govern when the scheduler should hand off to a
//!   stronger worker or a human.
//! - [`WorkerProfileRef`] is a lightweight, serializable reference to a profile
//!   by identifier. Specs (such as [`ExternalAgentSpec`](super::ExternalAgentSpec))
//!   store the *ref*; the heavy profile data lives in a registry, mirroring the
//!   [`ToolSetRef`](crate::agent::spec::ToolSetRef) / tool-registry split.
//! - [`WorkerProfileRegistry`] is the in-memory owner that maps an id back to a
//!   [`WorkerProfile`].
//!
//! Worktree isolation (design §10) is derived from the price band rather than
//! stored on the profile: [`CostTier::recommended_isolation`] gives a stronger
//! (more expensive) worker a more isolated worktree so concurrent edits do not
//! collide, and the bare [`WorktreeIsolation::default`](super::WorktreeIsolation)
//! isolates each agent by default.

use crate::agent::external::WorktreeIsolation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A class of work a worker advertises it can handle (design §9 "任务类型").
///
/// These tags let a capability-aware dispatcher match a task against the workers
/// able to perform it. [`Custom`](Self::Custom) is a provider-neutral escape
/// hatch for host-defined capabilities not named here, mirroring
/// [`ExternalRuntimeKind::Custom`](super::ExternalRuntimeKind::Custom).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Searching / locating code across the project.
    Search,
    /// Running shell commands.
    Shell,
    /// Writing or running tests.
    Test,
    /// Fixing an existing bug.
    BugFix,
    /// Implementing a new feature.
    Feature,
    /// Refactoring existing code without behaviour change.
    Refactor,
    /// Reviewing changes for quality, architecture, or security.
    Review,
    /// Investigating failures with an uncertain reproduction path.
    Debug,
    /// Generating new code from a specification.
    CodeGeneration,
    /// High-level planning / task decomposition.
    Planning,
    /// A host-defined capability identified by a free-form label.
    Custom(String),
}

/// Price band of a worker, ordered cheapest-to-most-expensive (design §9 预算).
///
/// The ordering is meaningful: a scheduler can compare tiers to decide whether a
/// candidate worker is *stronger* (more capable but pricier) than another, and
/// [`recommended_isolation`](Self::recommended_isolation) grows the worktree
/// isolation level with the tier. The default is [`Cheap`](Self::Cheap): a
/// cost-aware scheduler prefers the cheapest worker and escalates only when
/// needed (design §9).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CostTier {
    /// Low-cost workers: search, simple shell, small mechanical edits (§8).
    #[default]
    Cheap,
    /// Mid-tier workers for moderate multi-file changes.
    Standard,
    /// Strong, expensive workers (Claude Code / Codex / OpenCode) for complex,
    /// multi-file implementation and hard debugging (§8).
    Premium,
}

impl CostTier {
    /// Returns the worktree isolation level recommended for this price band
    /// (design §10).
    ///
    /// Stronger, more expensive workers get more isolation so concurrent edits
    /// do not collide: a [`Premium`](Self::Premium) worker defaults to an
    /// independent [`EphemeralGitWorktree`](WorktreeIsolation::EphemeralGitWorktree),
    /// a [`Standard`](Self::Standard) worker to a
    /// [`PerAgentWorktree`](WorktreeIsolation::PerAgentWorktree), and only a
    /// [`Cheap`](Self::Cheap) worker (typically read-heavy or making small
    /// mechanical edits) may share a worktree.
    #[must_use]
    pub const fn recommended_isolation(self) -> WorktreeIsolation {
        match self {
            Self::Cheap => WorktreeIsolation::Shared,
            Self::Standard => WorktreeIsolation::PerAgentWorktree,
            Self::Premium => WorktreeIsolation::EphemeralGitWorktree,
        }
    }

    /// Returns `true` when this tier is strictly more expensive (stronger) than
    /// `other`, useful for deciding whether an escalation target is an upgrade.
    #[must_use]
    pub fn is_stronger_than(self, other: Self) -> bool {
        self > other
    }
}

/// A condition under which the scheduler should escalate off a worker (§9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationTrigger {
    /// The worker exceeded its wall-clock / step budget without finishing.
    Timeout,
    /// Tests failed after the worker's changes.
    TestFailure,
    /// The worker self-reported low confidence in its result.
    LowConfidence,
    /// A reviewer rejected the worker's output (architecture / security issue).
    ReviewRejected,
    /// The remaining budget is too low to keep this worker running.
    BudgetExhausted,
}

/// Rules governing how the scheduler escalates away from a worker (design §9).
///
/// This is data only; the dispatcher (Milestone 6-2) and escalation logic
/// (Milestone 6-4) interpret it. A profile with an empty
/// [`EscalationRules`] (see [`none`](Self::none) / [`Default`]) is terminal: the
/// scheduler does not automatically re-dispatch off it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationRules {
    /// Conditions that should trigger an escalation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    triggers: Vec<EscalationTrigger>,
    /// The stronger worker to hand off to when a trigger fires, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    escalate_to: Option<WorkerProfileRef>,
    /// Whether to fall back to a human gate when no stronger worker resolves.
    #[serde(default, skip_serializing_if = "is_false")]
    human_fallback: bool,
}

impl EscalationRules {
    /// Creates escalation rules from explicit triggers and an optional target.
    #[must_use]
    pub fn new(
        triggers: impl IntoIterator<Item = EscalationTrigger>,
        escalate_to: Option<WorkerProfileRef>,
        human_fallback: bool,
    ) -> Self {
        Self {
            triggers: triggers.into_iter().collect(),
            escalate_to,
            human_fallback,
        }
    }

    /// Returns terminal rules that never escalate (equivalent to [`Default`]).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Returns the conditions that trigger escalation.
    #[must_use]
    pub fn triggers(&self) -> &[EscalationTrigger] {
        &self.triggers
    }

    /// Returns the worker to escalate to, if one is configured.
    #[must_use]
    pub const fn escalate_to(&self) -> Option<&WorkerProfileRef> {
        self.escalate_to.as_ref()
    }

    /// Returns whether escalation may fall back to a human gate.
    #[must_use]
    pub const fn human_fallback(&self) -> bool {
        self.human_fallback
    }

    /// Returns `true` when `trigger` is one of the configured escalation
    /// conditions.
    #[must_use]
    pub fn triggers_on(&self, trigger: EscalationTrigger) -> bool {
        self.triggers.contains(&trigger)
    }
}

/// Lightweight, serializable reference to a [`WorkerProfile`] by identifier.
///
/// A spec stores the *ref*; the full profile lives in a
/// [`WorkerProfileRegistry`]. This mirrors the
/// [`ToolSetRef`](crate::agent::spec::ToolSetRef) / registry split and keeps
/// heavy scheduling data out of persisted spec snapshots.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkerProfileRef {
    id: String,
}

impl WorkerProfileRef {
    /// Creates a worker-profile reference from a caller-supplied identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    /// Returns the referenced worker-profile identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Full, data-only description of one schedulable worker (design §9).
///
/// A `WorkerProfile` records the [`Capability`] tags a worker advertises, its
/// [`CostTier`] price band, and the [`EscalationRules`] that decide when to hand
/// off to a stronger worker or a human. Worktree isolation is *not* a field: it
/// is derived from the price band via
/// [`recommended_isolation`](Self::recommended_isolation) (design §10). Profiles
/// are held by a [`WorkerProfileRegistry`] and referenced from specs by
/// [`WorkerProfileRef`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerProfile {
    id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    capabilities: Vec<Capability>,
    cost_tier: CostTier,
    #[serde(default, skip_serializing_if = "EscalationRules::is_empty")]
    escalation: EscalationRules,
}

impl WorkerProfile {
    /// Creates a worker profile from its identity, capabilities, price band, and
    /// escalation rules.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        capabilities: impl IntoIterator<Item = Capability>,
        cost_tier: CostTier,
        escalation: EscalationRules,
    ) -> Self {
        Self {
            id: id.into(),
            capabilities: capabilities.into_iter().collect(),
            cost_tier,
            escalation,
        }
    }

    /// Returns the profile identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns a [`WorkerProfileRef`] pointing at this profile.
    #[must_use]
    pub fn reference(&self) -> WorkerProfileRef {
        WorkerProfileRef::new(self.id.clone())
    }

    /// Returns the capability tags this worker advertises.
    #[must_use]
    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    /// Returns the worker's price band.
    #[must_use]
    pub const fn cost_tier(&self) -> CostTier {
        self.cost_tier
    }

    /// Returns the worker's escalation rules.
    #[must_use]
    pub const fn escalation(&self) -> &EscalationRules {
        &self.escalation
    }

    /// Returns `true` when this worker advertises `capability`.
    #[must_use]
    pub fn has_capability(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }

    /// Returns the worktree isolation level recommended for this worker, derived
    /// from its [`CostTier`] (design §10).
    #[must_use]
    pub const fn recommended_isolation(&self) -> WorktreeIsolation {
        self.cost_tier.recommended_isolation()
    }
}

/// In-memory registry that owns [`WorkerProfile`]s and resolves them by
/// [`WorkerProfileRef`].
///
/// The registry is the heavy side of the ref / registry split: specs carry a
/// [`WorkerProfileRef`] while the registry holds the full profile data, so a
/// dispatcher can look up capability and cost information without embedding it in
/// every persisted spec.
#[derive(Clone, Debug, Default)]
pub struct WorkerProfileRegistry {
    profiles: BTreeMap<String, WorkerProfile>,
}

impl WorkerProfileRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `profile`, returning a [`WorkerProfileRef`] pointing at it.
    ///
    /// If a profile with the same id was already registered it is replaced; the
    /// returned ref always resolves to the newly stored profile.
    pub fn register(&mut self, profile: WorkerProfile) -> WorkerProfileRef {
        let reference = profile.reference();
        self.profiles.insert(profile.id().to_owned(), profile);
        reference
    }

    /// Resolves a [`WorkerProfileRef`] to its [`WorkerProfile`], or `None` when
    /// the ref is not registered.
    #[must_use]
    pub fn resolve(&self, reference: &WorkerProfileRef) -> Option<&WorkerProfile> {
        self.get(reference.id())
    }

    /// Looks up a profile by its raw identifier.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&WorkerProfile> {
        self.profiles.get(id)
    }

    /// Returns `true` when a profile for `reference` is registered.
    #[must_use]
    pub fn contains(&self, reference: &WorkerProfileRef) -> bool {
        self.profiles.contains_key(reference.id())
    }

    /// Returns the number of registered profiles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Returns `true` when no profiles are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Iterates over the registered profiles in ascending id order.
    pub fn iter(&self) -> impl Iterator<Item = &WorkerProfile> {
        self.profiles.values()
    }
}

impl EscalationRules {
    /// Serde predicate: treats terminal (no-op) rules as absent so a common
    /// profile snapshot stays compact.
    fn is_empty(&self) -> bool {
        self.triggers.is_empty() && self.escalate_to.is_none() && !self.human_fallback
    }
}

/// Serde predicate mirroring the crate-wide "skip `false` flags" convention.
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::{
        Capability, CostTier, EscalationRules, EscalationTrigger, WorkerProfile, WorkerProfileRef,
        WorkerProfileRegistry,
    };
    use crate::agent::external::WorktreeIsolation;
    use serde::{Serialize, de::DeserializeOwned};
    use std::fmt::Debug;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let restored: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&restored, value, "serde round-trip mismatch");
        restored
    }

    fn premium_profile() -> WorkerProfile {
        WorkerProfile::new(
            "cc-agent",
            [Capability::Feature, Capability::Debug, Capability::Refactor],
            CostTier::Premium,
            EscalationRules::new(
                [
                    EscalationTrigger::TestFailure,
                    EscalationTrigger::LowConfidence,
                ],
                Some(WorkerProfileRef::new("human-gate")),
                true,
            ),
        )
    }

    #[test]
    fn worker_profile_registry_register_and_resolve() {
        let mut registry = WorkerProfileRegistry::new();
        assert!(registry.is_empty());

        let cheap = WorkerProfile::new(
            "internal-cheap",
            [Capability::Search, Capability::Shell],
            CostTier::Cheap,
            EscalationRules::none(),
        );
        let cheap_ref = registry.register(cheap.clone());
        let premium_ref = registry.register(premium_profile());

        assert_eq!(registry.len(), 2);
        assert_eq!(cheap_ref, WorkerProfileRef::new("internal-cheap"));
        assert!(registry.contains(&premium_ref));

        let resolved = registry
            .resolve(&cheap_ref)
            .expect("cheap profile resolves");
        assert_eq!(resolved, &cheap);
        assert!(resolved.has_capability(&Capability::Search));
        assert!(!resolved.has_capability(&Capability::Review));

        assert_eq!(
            registry.get("cc-agent").map(WorkerProfile::cost_tier),
            Some(CostTier::Premium)
        );
    }

    #[test]
    fn worker_profile_registry_resolves_unknown_ref_to_none() {
        let registry = WorkerProfileRegistry::new();
        let missing = WorkerProfileRef::new("does-not-exist");
        assert!(registry.resolve(&missing).is_none());
        assert!(!registry.contains(&missing));
    }

    #[test]
    fn worker_profile_registry_register_replaces_same_id() {
        let mut registry = WorkerProfileRegistry::new();
        registry.register(WorkerProfile::new(
            "worker",
            [Capability::Search],
            CostTier::Cheap,
            EscalationRules::none(),
        ));
        let updated_ref = registry.register(WorkerProfile::new(
            "worker",
            [Capability::Feature],
            CostTier::Premium,
            EscalationRules::none(),
        ));

        assert_eq!(registry.len(), 1);
        let resolved = registry.resolve(&updated_ref).expect("resolves");
        assert_eq!(resolved.cost_tier(), CostTier::Premium);
        assert!(resolved.has_capability(&Capability::Feature));
    }

    #[test]
    fn worker_profile_serde_round_trip() {
        let profile = premium_profile();
        let restored = round_trip(&profile);
        assert_eq!(restored.reference(), WorkerProfileRef::new("cc-agent"));
        assert!(
            restored
                .escalation()
                .triggers_on(EscalationTrigger::TestFailure)
        );
        assert_eq!(
            restored.escalation().escalate_to(),
            Some(&WorkerProfileRef::new("human-gate"))
        );
        assert!(restored.escalation().human_fallback());

        round_trip(&Capability::Custom("mcp-only".to_owned()));
        round_trip(&CostTier::Standard);
        round_trip(&EscalationTrigger::BudgetExhausted);
    }

    #[test]
    fn worker_profile_terminal_escalation_serializes_compactly() {
        let profile = WorkerProfile::new(
            "explorer",
            [Capability::Search],
            CostTier::Cheap,
            EscalationRules::none(),
        );
        let json = serde_json::to_string(&profile).expect("serialize");
        assert!(
            !json.contains("escalation"),
            "terminal rules omitted: {json}"
        );
        assert_eq!(round_trip(&profile), profile);
    }

    #[test]
    fn worker_profile_worktree_isolation_default_policy() {
        // Bare default isolates each agent (design §10 "默认 worktree 隔离").
        assert_eq!(
            WorktreeIsolation::default(),
            WorktreeIsolation::PerAgentWorktree
        );

        // Cost-tier policy: stronger workers get more isolation, so a strong
        // (Premium) worker defaults to its own independent worktree (§10).
        assert_eq!(
            CostTier::Cheap.recommended_isolation(),
            WorktreeIsolation::Shared
        );
        assert_eq!(
            CostTier::Standard.recommended_isolation(),
            WorktreeIsolation::PerAgentWorktree
        );
        assert_eq!(
            CostTier::Premium.recommended_isolation(),
            WorktreeIsolation::EphemeralGitWorktree
        );

        // A Premium profile inherits the strong-worker isolation default.
        assert_eq!(
            premium_profile().recommended_isolation(),
            WorktreeIsolation::EphemeralGitWorktree
        );
    }

    #[test]
    fn worker_profile_cost_tier_ordering_and_default() {
        assert_eq!(CostTier::default(), CostTier::Cheap);
        assert!(CostTier::Cheap < CostTier::Standard);
        assert!(CostTier::Standard < CostTier::Premium);
        assert!(CostTier::Premium.is_stronger_than(CostTier::Cheap));
        assert!(!CostTier::Cheap.is_stronger_than(CostTier::Premium));
    }
}
