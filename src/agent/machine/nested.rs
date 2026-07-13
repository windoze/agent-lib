//! Nested Agent machines: an Agent and its subagents as one serializable tree.
//!
//! Migration doc §9 / effect-model §7.1 take the position that an `agent + its
//! subagents` hierarchy is **one nested machine** — the parent machine's state
//! *contains* its child machines — rather than a bag of sibling machines the
//! driver juggles. [`NestedMachine`] is that structure: each node owns a
//! [`DefaultAgentMachine`] plus a slot-keyed map of child [`NestedMachine`]s, so
//! the whole tree is a single value.
//!
//! # What a step does
//!
//! A [`step`](AgentMachine::step) advances the tree toward quiescence and
//! aggregates the requirements newly blocked this step from *anywhere* in the
//! tree, each stamped with its real [`AgentPath`] origin (effect-model §7.1):
//!
//! - [`StepInput::External`] feeds this node's own machine, then starts any
//!   freshly attached (not-yet-opened) children with their pending opening
//!   input — one `feed` at the root advances the whole tree.
//! - [`StepInput::Resume`] / [`StepInput::Abandon`] route to the node whose
//!   cursor is stuck on the addressed [`RequirementId`] (this node's own machine
//!   or a descendant subtree), so a fulfilled result reaches exactly the machine
//!   that emitted the requirement.
//!
//! Each node knows its absolute [`AgentPath`] in the tree and stamps both its
//! own machine's freshly emitted requirements and its persisted cursor binding
//! with that path, so an aggregated batch carries the true path from the root
//! to each emitting node and a restored cursor records where it sits. Routing
//! is by [`RequirementId`]: ids are host-supplied and unique, so a resolution
//! is delivered to the one node whose cursor holds that id.
//!
//! # Persistence boundary
//!
//! The tree is serializable: [`NestedMachine`] serializes as a
//! [`MachineTreeState`] — each node's data-only [`AgentState`] (which carries its
//! [`LoopCursor`]) plus its children — while every live handle (client, tool
//! registry, id source, approval policy) stays on the driver side. A restore
//! rebuilds the live tree from a [`MachineTreeState`] with
//! [`NestedMachine::from_state`], re-injecting handles per node.

use crate::agent::{
    AgentInput, AgentMachine, AgentPath, AgentSlot, AgentState, DefaultAgentMachine, LoopCursor,
    Notification, Requirement, RequirementId, StepInput, StepOutcome,
};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};
use std::collections::BTreeMap;

/// A node in a nested Agent machine: this node's own machine plus its children.
///
/// See the module-level documentation for the tree-step and persistence
/// contract.
#[derive(Debug)]
pub struct NestedMachine {
    /// This node's own sans-io machine (the root of this subtree).
    own: DefaultAgentMachine,
    /// Child subtrees keyed by the slot they occupy in this node.
    children: BTreeMap<AgentSlot, ChildNode>,
    /// This node's absolute path from the tree root. The own machine stamps its
    /// cursor and requirements at the root; this node re-bases them onto `path`
    /// so both the persisted cursor and the aggregated requirements carry the
    /// real [`AgentPath`] (migration doc §7.1).
    path: AgentPath,
}

/// A child subtree together with the opening input it still owes.
#[derive(Debug)]
struct ChildNode {
    /// The child's nested machine.
    machine: NestedMachine,
    /// The opening input fed to the child on the next [`NestedMachine::step`],
    /// then cleared. `None` once the child's turn has been opened.
    pending_start: Option<AgentInput>,
}

impl NestedMachine {
    /// Creates a root leaf node from a node machine, with no children.
    #[must_use]
    pub fn new(own: DefaultAgentMachine) -> Self {
        Self {
            own,
            children: BTreeMap::new(),
            path: AgentPath::root(),
        }
    }

    /// Attaches `child` at `slot`, to be opened with `opening` on the next step.
    ///
    /// The child is not driven immediately: it opens its turn when this node is
    /// next [`step`](AgentMachine::step)ped (any input), at which point its
    /// opening requirement is aggregated with real path `[slot, ..]`. This is
    /// the tree-growth primitive the subagent handler builds on (M5-2).
    ///
    /// # Errors
    ///
    /// Returns [`NestedMachineError::SlotOccupied`] when `slot` already holds a
    /// child.
    pub fn attach_child(
        &mut self,
        slot: AgentSlot,
        mut child: NestedMachine,
        opening: AgentInput,
    ) -> Result<(), NestedMachineError> {
        if self.children.contains_key(&slot) {
            return Err(NestedMachineError::SlotOccupied { slot });
        }
        // Re-base the attached subtree onto its real path under this node so its
        // cursors and requirements address it from the tree root.
        child.set_base(self.path.child(slot));
        self.children.insert(
            slot,
            ChildNode {
                machine: child,
                pending_start: Some(opening),
            },
        );
        Ok(())
    }

    /// Returns a read-only view of this node's own machine.
    #[must_use]
    pub const fn own(&self) -> &DefaultAgentMachine {
        &self.own
    }

    /// Returns the child subtree at `slot`, if present.
    #[must_use]
    pub fn child(&self, slot: AgentSlot) -> Option<&NestedMachine> {
        self.children.get(&slot).map(|child| &child.machine)
    }

    /// Returns the slots currently occupied by children, ascending.
    pub fn child_slots(&self) -> impl Iterator<Item = AgentSlot> + '_ {
        self.children.keys().copied()
    }

    /// Returns every outstanding requirement across the tree as an
    /// `(id, absolute-path)` pair.
    ///
    /// The path is reconstructed from the tree structure (the slot keys); it
    /// matches the real [`AgentPath`] each node also stamps into its own cursor
    /// binding, so the two views agree.
    #[must_use]
    pub fn outstanding_requirements(&self) -> Vec<(RequirementId, AgentPath)> {
        let mut out = Vec::new();
        self.collect_outstanding(&AgentPath::root(), &mut out);
        out
    }

    /// Rebuilds a live tree from a [`MachineTreeState`], re-injecting handles.
    ///
    /// `make` turns each node's restored [`AgentState`] into a live
    /// [`DefaultAgentMachine`] (re-attaching the client id source and any tool /
    /// approval handles). Children keep the opening input they had not yet been
    /// fed when the snapshot was taken.
    pub fn from_state<F>(state: MachineTreeState, make: &F) -> Self
    where
        F: Fn(AgentState) -> DefaultAgentMachine,
    {
        Self::from_state_at(state, AgentPath::root(), make)
    }

    /// Rebuilds a subtree rooted at absolute `base` from a [`MachineTreeState`].
    fn from_state_at<F>(state: MachineTreeState, base: AgentPath, make: &F) -> Self
    where
        F: Fn(AgentState) -> DefaultAgentMachine,
    {
        let own = make(state.node);
        let children = state
            .children
            .into_iter()
            .map(|(slot, child)| {
                (
                    slot,
                    ChildNode {
                        machine: Self::from_state_at(child.machine, base.child(slot), make),
                        pending_start: child.pending_start,
                    },
                )
            })
            .collect();
        Self {
            own,
            children,
            path: base,
        }
    }

    /// Re-bases this subtree onto absolute path `base`, re-stamping every node's
    /// cursor so its persisted binding records the node's real path.
    fn set_base(&mut self, base: AgentPath) {
        self.own.rebase_cursor_origin(&base);
        for (slot, child) in &mut self.children {
            child.machine.set_base(base.child(*slot));
        }
        self.path = base;
    }

    /// Steps this node's own machine, then stamps its freshly emitted
    /// requirements and its cursor binding with this node's real path.
    fn step_own(&mut self, input: StepInput) -> StepOutcome {
        let mut outcome = self.own.step(input);
        stamp_requirements(&mut outcome.requirements, &self.path);
        self.own.rebase_cursor_origin(&self.path);
        outcome
    }

    /// Whether any node in the tree still owes a child an opening input.
    fn has_pending_starts(&self) -> bool {
        self.children
            .values()
            .any(|child| child.pending_start.is_some() || child.machine.has_pending_starts())
    }

    /// Whether this node's own machine or any descendant is stuck on `id`.
    fn subtree_contains(&self, id: RequirementId) -> bool {
        self.own.cursor().pending_requirement_ids().contains(&id)
            || self
                .children
                .values()
                .any(|child| child.machine.subtree_contains(id))
    }

    /// Collects `(id, path)` for every outstanding requirement under `prefix`.
    fn collect_outstanding(&self, prefix: &AgentPath, out: &mut Vec<(RequirementId, AgentPath)>) {
        for id in self.own.cursor().pending_requirement_ids() {
            out.push((id, prefix.clone()));
        }
        for (slot, child) in &self.children {
            child.machine.collect_outstanding(&prefix.child(*slot), out);
        }
    }

    /// Routes a resume/abandon input to the node stuck on `id`, or to this
    /// node's own machine when no node awaits it (which surfaces a classified
    /// error from the own machine).
    fn route_by_id(
        &mut self,
        id: RequirementId,
        input: StepInput,
        notifications: &mut Vec<Notification>,
        requirements: &mut Vec<Requirement>,
    ) {
        if self.own.cursor().pending_requirement_ids().contains(&id) {
            let mut outcome = self.step_own(input);
            notifications.append(&mut outcome.notifications);
            requirements.append(&mut outcome.requirements);
            return;
        }

        let target = self
            .children
            .iter()
            .find_map(|(slot, child)| child.machine.subtree_contains(id).then_some(*slot));

        match target {
            Some(slot) => {
                let child = self
                    .children
                    .get_mut(&slot)
                    .expect("located child slot is present");
                // The child stamps its own subtree's real paths, so its outcome
                // already carries absolute origins; append it unchanged.
                let mut outcome = child.machine.step(input);
                notifications.append(&mut outcome.notifications);
                requirements.append(&mut outcome.requirements);
            }
            None => {
                // No node awaits this id; let the own machine classify the error.
                let mut outcome = self.step_own(input);
                notifications.append(&mut outcome.notifications);
                requirements.append(&mut outcome.requirements);
            }
        }
    }

    /// Opens any not-yet-started children with their pending opening input and
    /// aggregates their newly blocked requirements. Each child stamps its own
    /// subtree's real paths, so their outcomes carry absolute origins already.
    fn start_pending_children(
        &mut self,
        notifications: &mut Vec<Notification>,
        requirements: &mut Vec<Requirement>,
    ) {
        for child in self.children.values_mut() {
            if let Some(opening) = child.pending_start.take() {
                let mut outcome = child.machine.step(StepInput::External(opening));
                notifications.append(&mut outcome.notifications);
                requirements.append(&mut outcome.requirements);
            }
        }
    }
}

impl AgentMachine for NestedMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        let mut notifications = Vec::new();
        let mut requirements = Vec::new();

        match input {
            StepInput::External(external) => {
                let mut outcome = self.step_own(StepInput::External(external));
                notifications.append(&mut outcome.notifications);
                requirements.append(&mut outcome.requirements);
            }
            StepInput::Resume(resolution) => {
                let id = resolution.id;
                self.route_by_id(
                    id,
                    StepInput::Resume(resolution),
                    &mut notifications,
                    &mut requirements,
                );
            }
            StepInput::Abandon(id) => {
                self.route_by_id(
                    id,
                    StepInput::Abandon(id),
                    &mut notifications,
                    &mut requirements,
                );
            }
        }

        // One feed advances the whole tree: start freshly attached children so
        // their opening requirements join this step's aggregated batch.
        self.start_pending_children(&mut notifications, &mut requirements);

        let quiescent = !self.has_pending_starts();
        StepOutcome::new(notifications, requirements, quiescent)
    }

    fn cursor(&self) -> &LoopCursor {
        self.own.cursor()
    }
}

/// Stamps requirements freshly emitted by a node's own machine with `path`,
/// the node's real [`AgentPath`]. The own machine emits them rooted, so this
/// records the true path from the tree root to the emitting node.
fn stamp_requirements(requirements: &mut [Requirement], path: &AgentPath) {
    for requirement in requirements.iter_mut() {
        requirement.origin = path.clone();
    }
}

impl Serialize for NestedMachine {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let children: BTreeMap<AgentSlot, ChildStateRef<'_>> = self
            .children
            .iter()
            .map(|(slot, child)| {
                (
                    *slot,
                    ChildStateRef {
                        machine: &child.machine,
                        pending_start: child.pending_start.as_ref(),
                    },
                )
            })
            .collect();

        let field_count = 1 + usize::from(!children.is_empty());
        let mut record = serializer.serialize_struct("MachineTreeState", field_count)?;
        record.serialize_field("node", self.own.state())?;
        if children.is_empty() {
            record.skip_field("children")?;
        } else {
            record.serialize_field("children", &children)?;
        }
        record.end()
    }
}

/// Borrowing view of a child node used only for serialization.
#[derive(Serialize)]
struct ChildStateRef<'a> {
    machine: &'a NestedMachine,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_start: Option<&'a AgentInput>,
}

/// Serializable snapshot of a whole nested-machine subtree.
///
/// This is the data-only shape [`NestedMachine`] serializes into: each node's
/// [`AgentState`] (which carries its [`LoopCursor`]) plus its children. Rebuild
/// a live tree with [`NestedMachine::from_state`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineTreeState {
    /// This node's own data-only Agent state.
    node: AgentState,
    /// Child subtrees keyed by slot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    children: BTreeMap<AgentSlot, ChildState>,
}

impl MachineTreeState {
    /// Returns this node's own restored Agent state.
    #[must_use]
    pub const fn node(&self) -> &AgentState {
        &self.node
    }

    /// Returns the restored child subtree at `slot`, if present.
    #[must_use]
    pub fn child(&self, slot: AgentSlot) -> Option<&MachineTreeState> {
        self.children.get(&slot).map(|child| &child.machine)
    }
}

/// Serializable child entry: a subtree plus its not-yet-fed opening input.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildState {
    machine: MachineTreeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_start: Option<AgentInput>,
}

/// Errors from composing a nested Agent machine tree.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NestedMachineError {
    /// A child was attached to a slot that already held one.
    #[error("child slot {slot} is already occupied")]
    SlotOccupied {
        /// The slot that was already taken.
        slot: AgentSlot,
    },
}

#[cfg(test)]
mod tests;
