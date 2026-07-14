//! Scripted subagent spawner and parent/child scope helpers built on
//! `agent-lib`'s [`SubagentSpawner`] / [`DrivingSubagentHandler`] pair.
//!
//! `NeedSubagent` is the one requirement that *deepens* the scope chain: the
//! reference [`DrivingSubagentHandler`] owns the depth / budget / cancel guards
//! and drives the child under a fresh drain layer, but delegates the *policy* of
//! turning a spec into a runnable child to a host [`SubagentSpawner`]. Agent-layer
//! tests otherwise hand-write that spawner, a child machine, and a child scope in
//! every file that exercises a hierarchy. This module collapses that boilerplate:
//!
//! - [`ScriptedSubagentSpawner`] is a ready-made [`SubagentSpawner`] that mints
//!   deterministic child ids from a [`SeqIds`](crate::ids::SeqIds) tree, hands
//!   back a pre-built or factory-built [`SpawnedChild`], and returns a scripted
//!   summary — while counting how often each hook (`child_ids` / `spawn` /
//!   `summarize`) was reached so a test can assert the guards fired (or did
//!   *not*).
//! - [`SpawnedChildBuilder`] composes the three pieces the handler needs — a child
//!   [`AgentMachine`], its own drain [`scope`](HandlerScope), and the
//!   [`AgentInput`] that opens its turn — into one [`SpawnedChild`].
//! - [`headless_child_scope`], [`attended_child_scope`], and
//!   [`parent_scope_with_subagent`] name the three recurring scope shapes: a child
//!   that *pops* its interaction out to the parent, a child that answers its own
//!   interaction in place, and a parent that serves `NeedSubagent`.
//!
//! Everything is provider-neutral and leans on [`ScriptMachine`](crate::machine::ScriptMachine),
//! [`SeqIds`](crate::ids::SeqIds), and [`TestScope`] rather than any concrete
//! machine internals, so a hierarchy test observes the *handler's* wiring
//! (derivation, nested drain, guards) without a live LLM or tool backend.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agent_lib::agent::{
    AgentError, AgentInput, AgentMachine, AgentSpecRef, DrivingSubagentHandler, HandlerScope,
    Interaction, InteractionHandler, RunId, SpawnedChild, SubagentOutput, SubagentSpawner,
    TraceNodeId, TurnDone,
};
use serde_json::Value;

use crate::scope::{TestScope, TestScopeBuilder};

/// A boxed factory that builds a fresh [`SpawnedChild`] on each `NeedSubagent`.
type ChildFactory = Box<dyn Fn() -> SpawnedChild + Send + Sync>;

/// Where a [`ScriptedSubagentSpawner`] draws each child from.
///
/// A [`Once`](ChildSource::Once) source hands back a single pre-built child (the
/// common case: a test builds the child machine outside, clones its
/// [`log`](crate::machine::ScriptMachine::log) to observe it after the drive,
/// then hands the whole [`SpawnedChild`] in). A [`Factory`](ChildSource::Factory)
/// source rebuilds a fresh child per spawn, for hierarchies that derive more than
/// one child.
enum ChildSource {
    /// A single child, taken on the first `spawn`.
    Once(Mutex<Option<SpawnedChild>>),
    /// A factory rebuilding a fresh child per `spawn`.
    Factory(ChildFactory),
}

/// A ready-made [`SubagentSpawner`] for hierarchy tests.
///
/// Build one with [`ScriptedSubagentSpawner::builder`]. It mints deterministic
/// child ids from a [`SeqIds`](crate::ids::SeqIds) tree, yields a [`SpawnedChild`]
/// from either a single pre-built child or a factory, and returns a scripted (or
/// fixed) summary. The three hook counters
/// ([`ids_calls`](Self::ids_calls), [`spawn_calls`](Self::spawn_calls),
/// [`summarize_calls`](Self::summarize_calls)) let a test assert exactly how far
/// the [`DrivingSubagentHandler`] got — for example that a tripped depth guard
/// reached *neither* `child_ids` nor `spawn`.
///
/// Wrap it in the reference handler with [`into_handler`](Self::into_handler);
/// keep your own [`Arc`] clone to read the counters back after the drive.
pub struct ScriptedSubagentSpawner {
    ids: crate::ids::SeqIds,
    trace_label: String,
    source: ChildSource,
    summaries: Mutex<VecDeque<String>>,
    default_summary: String,
    briefs: Mutex<Vec<Interaction>>,
    ids_calls: AtomicUsize,
    spawn_calls: AtomicUsize,
    summarize_calls: AtomicUsize,
}

impl ScriptedSubagentSpawner {
    /// Starts a builder that mints child ids from `ids`.
    #[must_use]
    pub fn builder(ids: crate::ids::SeqIds) -> ScriptedSubagentSpawnerBuilder {
        ScriptedSubagentSpawnerBuilder::new(ids)
    }

    /// Returns the id source child ids are minted from.
    #[must_use]
    pub fn ids(&self) -> &crate::ids::SeqIds {
        &self.ids
    }

    /// Returns how many times [`child_ids`](SubagentSpawner::child_ids) was
    /// reached (once per derivation the handler actually attempted).
    #[must_use]
    pub fn ids_calls(&self) -> usize {
        self.ids_calls.load(Ordering::SeqCst)
    }

    /// Returns how many times [`spawn`](SubagentSpawner::spawn) was reached (once
    /// per child the handler actually built).
    #[must_use]
    pub fn spawn_calls(&self) -> usize {
        self.spawn_calls.load(Ordering::SeqCst)
    }

    /// Returns how many times [`summarize`](SubagentSpawner::summarize) was
    /// reached (once per child that ran to completion).
    #[must_use]
    pub fn summarize_calls(&self) -> usize {
        self.summarize_calls.load(Ordering::SeqCst)
    }

    /// Returns, in `spawn` order, every brief the handler handed this spawner.
    ///
    /// The reference [`DrivingSubagentHandler`] passes the emitting
    /// `NeedSubagent`'s brief straight through to [`spawn`](SubagentSpawner::spawn),
    /// so this is where a test observes that the brief the parent produced (for
    /// example after a mid-turn pivot re-rendered the parent's request) actually
    /// reached the child derivation, rather than a stale opening goal.
    #[must_use]
    pub fn briefs(&self) -> Vec<Interaction> {
        self.briefs.lock().expect("briefs mutex").clone()
    }

    /// Wraps this spawner in a [`DrivingSubagentHandler`] that refuses to derive a
    /// child at depth `>= max_depth`.
    ///
    /// Takes `Arc<Self>` so a test can keep its own clone to read the hook
    /// counters back after the drive.
    #[must_use]
    pub fn into_handler(self: Arc<Self>, max_depth: u32) -> DrivingSubagentHandler {
        DrivingSubagentHandler::new(self, max_depth)
    }
}

impl SubagentSpawner for ScriptedSubagentSpawner {
    fn child_ids(&self, _spec_ref: &AgentSpecRef) -> Result<(RunId, TraceNodeId), AgentError> {
        self.ids_calls.fetch_add(1, Ordering::SeqCst);
        Ok((self.ids.run_id(), self.ids.trace_node(&self.trace_label)))
    }

    fn spawn(
        &self,
        _spec_ref: &AgentSpecRef,
        brief: &Interaction,
        _result_schema: Option<&Value>,
    ) -> Result<SpawnedChild, AgentError> {
        self.briefs
            .lock()
            .expect("briefs mutex")
            .push(brief.clone());
        let nth = self.spawn_calls.fetch_add(1, Ordering::SeqCst);
        match &self.source {
            ChildSource::Factory(factory) => Ok(factory()),
            ChildSource::Once(slot) => Ok(slot
                .lock()
                .expect("child slot mutex")
                .take()
                .unwrap_or_else(|| {
                    panic!(
                        "ScriptedSubagentSpawner built with a single `.child(..)` was asked to \
                         spawn {} times; use `.child_factory(..)` for a hierarchy that derives \
                         more than one child",
                        nth + 1
                    )
                })),
        }
    }

    fn summarize(&self, _done: &TurnDone) -> SubagentOutput {
        self.summarize_calls.fetch_add(1, Ordering::SeqCst);
        let summary = self
            .summaries
            .lock()
            .expect("summaries mutex")
            .pop_front()
            .unwrap_or_else(|| self.default_summary.clone());
        SubagentOutput { summary }
    }
}

/// A fluent builder for [`ScriptedSubagentSpawner`].
///
/// A child source is required — call exactly one of [`child`](Self::child) or
/// [`child_factory`](Self::child_factory). Every other knob has a default: the
/// trace-node label is `"child"`, and the summary is `"child summary"` until
/// overridden with [`summary`](Self::summary) or scripted with
/// [`summaries`](Self::summaries).
pub struct ScriptedSubagentSpawnerBuilder {
    ids: crate::ids::SeqIds,
    trace_label: String,
    source: Option<ChildSource>,
    summaries: VecDeque<String>,
    default_summary: String,
}

impl ScriptedSubagentSpawnerBuilder {
    /// Creates a builder minting child ids from `ids`, with default knobs.
    #[must_use]
    pub fn new(ids: crate::ids::SeqIds) -> Self {
        Self {
            ids,
            trace_label: "child".to_owned(),
            source: None,
            summaries: VecDeque::new(),
            default_summary: "child summary".to_owned(),
        }
    }

    /// Sets the `node` label used to mint the child's sub-agent
    /// [`TraceNodeId`].
    #[must_use]
    pub fn trace_label(mut self, label: impl Into<String>) -> Self {
        self.trace_label = label.into();
        self
    }

    /// Hands back `child` on the first (and only) `spawn`.
    ///
    /// The common single-subagent case: build the child machine first, clone its
    /// [`log`](crate::machine::ScriptMachine::log) to observe it after the drive,
    /// then hand the whole [`SpawnedChild`] in here. A second `spawn` panics — use
    /// [`child_factory`](Self::child_factory) for a hierarchy with more children.
    #[must_use]
    pub fn child(mut self, child: SpawnedChild) -> Self {
        self.source = Some(ChildSource::Once(Mutex::new(Some(child))));
        self
    }

    /// Rebuilds a fresh child from `factory` on every `spawn`.
    ///
    /// Use for hierarchies that derive more than one child, or to assert a child
    /// is *never* built (pass a factory that panics and check
    /// [`spawn_calls`](ScriptedSubagentSpawner::spawn_calls) stays zero).
    #[must_use]
    pub fn child_factory(
        mut self,
        factory: impl Fn() -> SpawnedChild + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(ChildSource::Factory(Box::new(factory)));
        self
    }

    /// Sets the summary returned once the scripted [`summaries`](Self::summaries)
    /// queue (if any) is drained.
    #[must_use]
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.default_summary = summary.into();
        self
    }

    /// Scripts one summary per finished child, returned in order.
    ///
    /// When the queue is drained, later children fall back to the fixed
    /// [`summary`](Self::summary).
    #[must_use]
    pub fn summaries(mut self, summaries: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.summaries = summaries.into_iter().map(Into::into).collect();
        self
    }

    /// Finalises the builder into a [`ScriptedSubagentSpawner`].
    ///
    /// # Panics
    ///
    /// Panics if no child source was set (call [`child`](Self::child) or
    /// [`child_factory`](Self::child_factory)).
    #[must_use]
    pub fn build(self) -> ScriptedSubagentSpawner {
        let source = self.source.expect(
            "ScriptedSubagentSpawner needs a child source: call `.child(..)` or \
             `.child_factory(..)`",
        );
        ScriptedSubagentSpawner {
            ids: self.ids,
            trace_label: self.trace_label,
            source,
            summaries: Mutex::new(self.summaries),
            default_summary: self.default_summary,
            briefs: Mutex::new(Vec::new()),
            ids_calls: AtomicUsize::new(0),
            spawn_calls: AtomicUsize::new(0),
            summarize_calls: AtomicUsize::new(0),
        }
    }
}

/// A fluent builder for a [`SpawnedChild`].
///
/// Composes the three pieces a [`DrivingSubagentHandler`] drives a child with: the
/// child [`machine`](Self::machine), its own drain [`scope`](Self::scope), and the
/// [`opening`](Self::opening) [`AgentInput`] that starts its turn. All three are
/// required.
#[derive(Default)]
pub struct SpawnedChildBuilder {
    machine: Option<Box<dyn AgentMachine + Send>>,
    scope: Option<Box<dyn HandlerScope>>,
    opening: Option<AgentInput>,
}

impl SpawnedChildBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the child machine, boxing it.
    #[must_use]
    pub fn machine(mut self, machine: impl AgentMachine + Send + 'static) -> Self {
        self.machine = Some(Box::new(machine));
        self
    }

    /// Sets the child machine from an already-boxed value.
    #[must_use]
    pub fn boxed_machine(mut self, machine: Box<dyn AgentMachine + Send>) -> Self {
        self.machine = Some(machine);
        self
    }

    /// Sets the child's own drain scope, boxing it.
    ///
    /// Whatever this scope does not serve pops out to the outer (parent) layer the
    /// handler drives the child under.
    #[must_use]
    pub fn scope(mut self, scope: impl HandlerScope + 'static) -> Self {
        self.scope = Some(Box::new(scope));
        self
    }

    /// Sets the child's drain scope from an already-boxed value.
    #[must_use]
    pub fn boxed_scope(mut self, scope: Box<dyn HandlerScope>) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Sets the [`AgentInput`] that opens the child's turn.
    #[must_use]
    pub fn opening(mut self, opening: AgentInput) -> Self {
        self.opening = Some(opening);
        self
    }

    /// Finalises the builder into a [`SpawnedChild`].
    ///
    /// # Panics
    ///
    /// Panics if the machine, scope, or opening input was not set.
    #[must_use]
    pub fn build(self) -> SpawnedChild {
        SpawnedChild {
            machine: self
                .machine
                .expect("SpawnedChildBuilder needs a machine: call `.machine(..)`"),
            scope: self
                .scope
                .expect("SpawnedChildBuilder needs a scope: call `.scope(..)`"),
            opening: self
                .opening
                .expect("SpawnedChildBuilder needs an opening input: call `.opening(..)`"),
        }
    }
}

/// Builds a *headless* child drain layer: it serves no interaction backend, so a
/// child `NeedInteraction` pops out to the outer (parent) layer instead of being
/// answered in place.
///
/// Attach the child's own effect families (`llm` / `tool` / …) on the returned
/// [`TestScopeBuilder`], then `.build()`. This is a `TestScope::builder()` with
/// intent named: a `TestScope` is headless by default.
#[must_use]
pub fn headless_child_scope() -> TestScopeBuilder {
    TestScope::builder()
}

/// Builds an *attended* child drain layer: it answers its own `NeedInteraction`
/// in place through `interaction`, so a child interaction never pops to the
/// parent.
///
/// Attach any further families on the returned [`TestScopeBuilder`], then
/// `.build()`.
#[must_use]
pub fn attended_child_scope(interaction: Arc<dyn InteractionHandler>) -> TestScopeBuilder {
    TestScope::builder().attended(interaction)
}

/// Builds a parent drain layer that serves `NeedSubagent` through `handler`.
///
/// Chain `.attended(..)` on the returned [`TestScopeBuilder`] to also serve a
/// headless child's popped `NeedInteraction`, or `.llm(..)` / `.tool(..)` for the
/// parent's own effects, then `.build()`.
#[must_use]
pub fn parent_scope_with_subagent(handler: DrivingSubagentHandler) -> TestScopeBuilder {
    TestScope::builder().subagent(Arc::new(handler))
}

#[cfg(test)]
mod tests;
