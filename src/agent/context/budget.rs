//! Budget limits, snapshots, and shared accounting handles.

use crate::model::usage::Usage;
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use thiserror::Error;

/// Serializable budget limits for one run lineage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_steps: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_wall_time: Option<Duration>,
}

impl BudgetLimits {
    /// Creates budget limits for step, token, cost, and wall-clock dimensions.
    #[must_use]
    pub const fn new(
        max_steps: Option<u64>,
        max_tokens: Option<u64>,
        max_cost_micros: Option<u64>,
        max_wall_time: Option<Duration>,
    ) -> Self {
        Self {
            max_steps,
            max_tokens,
            max_cost_micros,
            max_wall_time,
        }
    }

    /// Returns an unbounded budget.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self::new(None, None, None, None)
    }

    /// Returns the maximum step count, if one is configured.
    #[must_use]
    pub const fn max_steps(&self) -> Option<u64> {
        self.max_steps
    }

    /// Returns the maximum token count, if one is configured.
    #[must_use]
    pub const fn max_tokens(&self) -> Option<u64> {
        self.max_tokens
    }

    /// Returns the maximum cost in micro-units, if one is configured.
    #[must_use]
    pub const fn max_cost_micros(&self) -> Option<u64> {
        self.max_cost_micros
    }

    /// Returns the maximum wall-clock elapsed time, if one is configured.
    #[must_use]
    pub const fn max_wall_time(&self) -> Option<Duration> {
        self.max_wall_time
    }
}

/// Serializable counters for consumed budget dimensions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsage {
    #[serde(default)]
    steps: u64,
    #[serde(default)]
    tokens: u64,
    #[serde(default)]
    cost_micros: u64,
}

impl BudgetUsage {
    /// Creates a consumed-budget record.
    #[must_use]
    pub const fn new(steps: u64, tokens: u64, cost_micros: u64) -> Self {
        Self {
            steps,
            tokens,
            cost_micros,
        }
    }

    /// Returns consumed logical Agent steps.
    #[must_use]
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// Returns consumed model tokens.
    #[must_use]
    pub const fn tokens(&self) -> u64 {
        self.tokens
    }

    /// Returns consumed cost in micro-units.
    #[must_use]
    pub const fn cost_micros(&self) -> u64 {
        self.cost_micros
    }
}

/// Serializable point-in-time budget record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    limits: BudgetLimits,
    used: BudgetUsage,
}

impl BudgetSnapshot {
    /// Creates an empty usage snapshot for the supplied limits.
    #[must_use]
    pub const fn new(limits: BudgetLimits) -> Self {
        Self {
            limits,
            used: BudgetUsage::new(0, 0, 0),
        }
    }

    /// Creates a budget snapshot from explicit limits and consumed counters.
    #[must_use]
    pub const fn from_parts(limits: BudgetLimits, used: BudgetUsage) -> Self {
        Self { limits, used }
    }

    /// Returns the configured budget limits.
    #[must_use]
    pub const fn limits(&self) -> &BudgetLimits {
        &self.limits
    }

    /// Returns the consumed budget counters.
    #[must_use]
    pub const fn used(&self) -> &BudgetUsage {
        &self.used
    }
}

/// Atomic budget charge applied to a shared [`BudgetHandle`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetCharge {
    steps: u64,
    tokens: u64,
    cost_micros: u64,
}

impl BudgetCharge {
    /// Creates a charge for step, token, and cost dimensions.
    #[must_use]
    pub const fn new(steps: u64, tokens: u64, cost_micros: u64) -> Self {
        Self {
            steps,
            tokens,
            cost_micros,
        }
    }

    /// Creates a charge for one logical Agent step.
    #[must_use]
    pub const fn step() -> Self {
        Self::new(1, 0, 0)
    }

    /// Creates a charge for a raw token count.
    #[must_use]
    pub const fn tokens(tokens: u64) -> Self {
        Self::new(0, tokens, 0)
    }

    /// Creates a charge for cost in micro-units.
    #[must_use]
    pub const fn cost_micros(cost_micros: u64) -> Self {
        Self::new(0, 0, cost_micros)
    }

    /// Returns charged logical Agent steps.
    #[must_use]
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// Returns charged model tokens.
    #[must_use]
    pub const fn token_count(&self) -> u64 {
        self.tokens
    }

    /// Returns charged cost in micro-units.
    #[must_use]
    pub const fn cost(&self) -> u64 {
        self.cost_micros
    }
}

/// Shared live handle for budget accounting.
///
/// Clones of this handle point to the same ledger. That is the mechanism used
/// by child contexts to consume the parent run's limits instead of creating a
/// fresh isolated budget.
#[derive(Clone, Debug)]
pub struct BudgetHandle {
    inner: Arc<Mutex<BudgetSnapshot>>,
}

impl BudgetHandle {
    /// Creates a shared budget handle from serializable limits.
    #[must_use]
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BudgetSnapshot::new(limits))),
        }
    }

    /// Restores a shared budget handle from a persisted snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: BudgetSnapshot) -> Self {
        Self {
            inner: Arc::new(Mutex::new(snapshot)),
        }
    }

    /// Returns the current serializable budget snapshot.
    #[must_use]
    pub fn snapshot(&self) -> BudgetSnapshot {
        *self.inner.lock().expect("budget mutex poisoned")
    }

    /// Returns the first configured count-like dimension that has no headroom.
    ///
    /// This is a best-effort preflight check for drivers before they start new
    /// spend. It is not a reservation: another holder of the same budget can
    /// still consume headroom before the eventual charge is applied.
    #[must_use]
    pub fn exhausted_dimension(&self) -> Option<BudgetDimension> {
        let snapshot = self.snapshot();
        let limits = snapshot.limits();
        let used = snapshot.used();
        let exhausted =
            |limit: Option<u64>, used: u64| -> bool { limit.is_some_and(|limit| used >= limit) };

        if exhausted(limits.max_steps(), used.steps()) {
            Some(BudgetDimension::Steps)
        } else if exhausted(limits.max_tokens(), used.tokens()) {
            Some(BudgetDimension::Tokens)
        } else if exhausted(limits.max_cost_micros(), used.cost_micros()) {
            Some(BudgetDimension::CostMicros)
        } else {
            None
        }
    }

    /// Applies a multi-dimensional charge atomically.
    ///
    /// No counter is changed if any charged dimension would exceed its limit.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError::Exceeded`] for a configured limit breach or
    /// [`BudgetError::CounterOverflow`] for `u64` addition overflow.
    pub fn try_charge(&self, charge: BudgetCharge) -> Result<BudgetSnapshot, BudgetError> {
        let mut snapshot = self.inner.lock().expect("budget mutex poisoned");
        let used = snapshot.used;
        let limits = snapshot.limits;

        let steps = checked_counter(
            BudgetDimension::Steps,
            used.steps,
            charge.steps,
            limits.max_steps,
        )?;
        let tokens = checked_counter(
            BudgetDimension::Tokens,
            used.tokens,
            charge.tokens,
            limits.max_tokens,
        )?;
        let cost_micros = checked_counter(
            BudgetDimension::CostMicros,
            used.cost_micros,
            charge.cost_micros,
            limits.max_cost_micros,
        )?;

        snapshot.used = BudgetUsage::new(steps, tokens, cost_micros);
        Ok(*snapshot)
    }

    /// Charges one logical Agent step.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError`] if the step charge cannot be applied.
    pub fn charge_step(&self) -> Result<BudgetSnapshot, BudgetError> {
        self.try_charge(BudgetCharge::step())
    }

    /// Charges raw model tokens.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError`] if the token charge cannot be applied.
    pub fn charge_tokens(&self, tokens: u64) -> Result<BudgetSnapshot, BudgetError> {
        self.try_charge(BudgetCharge::tokens(tokens))
    }

    /// Charges the token count represented by normalized usage.
    ///
    /// If a provider reported a total, that value is used. Otherwise the total
    /// is computed from normalized input, output, cache, and reasoning fields.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError`] if the token charge cannot be applied.
    pub fn charge_usage(&self, usage: &Usage) -> Result<BudgetSnapshot, BudgetError> {
        self.charge_tokens(u64::from(
            usage.total.unwrap_or_else(|| usage.total_computed()),
        ))
    }

    /// Charges cost in micro-units.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError`] if the cost charge cannot be applied.
    pub fn charge_cost_micros(&self, cost_micros: u64) -> Result<BudgetSnapshot, BudgetError> {
        self.try_charge(BudgetCharge::cost_micros(cost_micros))
    }

    /// Checks externally measured wall-clock elapsed time.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetError::WallClockExceeded`] when `elapsed` is greater
    /// than the configured wall-clock limit.
    pub fn check_wall_clock(&self, elapsed: Duration) -> Result<(), BudgetError> {
        let snapshot = self.inner.lock().expect("budget mutex poisoned");
        if let Some(limit) = snapshot.limits.max_wall_time
            && elapsed > limit
        {
            return Err(BudgetError::WallClockExceeded { limit, elapsed });
        }
        Ok(())
    }
}

/// Budget dimension reported in classified errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    /// Logical Agent step count.
    Steps,
    /// Model token count.
    Tokens,
    /// Cost in micro-units.
    CostMicros,
    /// Wall-clock elapsed time.
    WallClock,
}

/// Classified budget failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum BudgetError {
    /// A configured count-like budget would be exceeded.
    #[error("{dimension:?} budget exceeded: limit {limit}, attempted {attempted}")]
    Exceeded {
        /// Dimension whose limit was reached.
        dimension: BudgetDimension,
        /// Configured limit.
        limit: u64,
        /// Counter value that would result from the attempted charge.
        attempted: u64,
        /// Remaining amount before the failed charge.
        remaining: u64,
    },
    /// A configured wall-clock budget has been exceeded.
    #[error("wall-clock budget exceeded: limit {limit:?}, elapsed {elapsed:?}")]
    WallClockExceeded {
        /// Configured wall-clock limit.
        limit: Duration,
        /// Caller-supplied elapsed time.
        elapsed: Duration,
    },
    /// A count-like budget counter would overflow `u64`.
    #[error("{dimension:?} budget counter overflow")]
    CounterOverflow {
        /// Dimension whose counter overflowed.
        dimension: BudgetDimension,
    },
}

fn checked_counter(
    dimension: BudgetDimension,
    current: u64,
    charge: u64,
    limit: Option<u64>,
) -> Result<u64, BudgetError> {
    let attempted = current
        .checked_add(charge)
        .ok_or(BudgetError::CounterOverflow { dimension })?;

    if let Some(limit) = limit
        && attempted > limit
    {
        return Err(BudgetError::Exceeded {
            dimension,
            limit,
            attempted,
            remaining: limit.saturating_sub(current),
        });
    }

    Ok(attempted)
}
