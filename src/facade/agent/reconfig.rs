//! Facade-specific tool-registry resolution for turn-boundary reconfiguration.
//!
//! The agent layer stores reconfiguration as data-only [`ToolSetRef`] values,
//! while the facade owns live typed-tool closures. This module bridges the two:
//! it resolves an active declaration set to a run-bound registry that advertises
//! exactly those declarations and only executes tools whose names remain active.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::{ToolRegistry, ToolRegistryResolver, ToolRuntimeError, ToolSetRef};
use crate::conversation::ToolCallId;
use crate::facade::tool::{FacadeToolRegistry, Tool, ToolContextParts};
use crate::model::tool::{Tool as ToolDecl, ToolCall, ToolResponse};

/// Resolves facade `ToolSetRef` reconfigurations against the executable tool
/// surface registered on the owning [`Agent`](super::Agent).
pub(super) struct FacadeToolRegistryResolver {
    tools: Arc<[Tool]>,
    custom: Option<Arc<dyn ToolRegistry>>,
    extra: Arc<[ToolDecl]>,
    available_names: BTreeSet<String>,
    context: Arc<Mutex<Option<ToolContextParts>>>,
}

impl FacadeToolRegistryResolver {
    /// Creates a resolver for one facade agent's fixed runtime tool surface.
    pub(super) fn new(
        tools: Arc<[Tool]>,
        custom: Option<Arc<dyn ToolRegistry>>,
        extra: Arc<[ToolDecl]>,
        available_declarations: Vec<ToolDecl>,
    ) -> Self {
        let available_names = available_declarations
            .iter()
            .map(|declaration| declaration.name.clone())
            .collect();
        Self {
            tools,
            custom,
            extra,
            available_names,
            context: Arc::new(Mutex::new(None)),
        }
    }

    /// Binds the run-scoped context used by any registry this resolver returns.
    pub(super) fn bind_context(&self, context: ToolContextParts) {
        *self
            .context
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(context);
    }

    /// Resolves the registry for the currently active tool set.
    pub(super) fn resolve_active_registry(
        &self,
        tool_set: &ToolSetRef,
    ) -> Result<Arc<dyn ToolRegistry>, ToolRuntimeError> {
        self.resolve_tool_set(tool_set)
    }

    fn validate_requested_names(&self, tool_set: &ToolSetRef) -> Result<(), ToolRuntimeError> {
        let mut requested = BTreeSet::new();
        let mut missing = Vec::new();
        for declaration in tool_set.tools() {
            if !requested.insert(declaration.name.as_str()) {
                return Err(ToolRuntimeError::InvalidRegistry {
                    message: format!(
                        "tool set {} repeats tool `{}`",
                        tool_set.id(),
                        declaration.name
                    ),
                });
            }
            if !self.available_names.contains(&declaration.name) {
                missing.push(declaration.name.clone());
            }
        }

        if missing.is_empty() {
            Ok(())
        } else {
            Err(ToolRuntimeError::InvalidRegistry {
                message: format!(
                    "tool set {} references tool(s) not present in the facade registry: {}",
                    tool_set.id(),
                    missing.join(", ")
                ),
            })
        }
    }
}

impl fmt::Debug for FacadeToolRegistryResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FacadeToolRegistryResolver")
            .field("available_names", &self.available_names)
            .field("has_custom", &self.custom.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolRegistryResolver for FacadeToolRegistryResolver {
    fn resolve_tool_set(
        &self,
        tool_set: &ToolSetRef,
    ) -> Result<Arc<dyn ToolRegistry>, ToolRuntimeError> {
        self.validate_requested_names(tool_set)?;
        let declarations = tool_set.tools().to_vec();
        let allowed_names = declarations
            .iter()
            .map(|declaration| declaration.name.clone())
            .collect();
        Ok(Arc::new(ActiveFacadeToolRegistry {
            tools: self.tools.clone(),
            custom: self.custom.clone(),
            extra: self.extra.clone(),
            context: self.context.clone(),
            declarations,
            allowed_names,
        }))
    }
}

/// Active, filtered view of the facade registry installed for one tool set.
struct ActiveFacadeToolRegistry {
    tools: Arc<[Tool]>,
    custom: Option<Arc<dyn ToolRegistry>>,
    extra: Arc<[ToolDecl]>,
    context: Arc<Mutex<Option<ToolContextParts>>>,
    declarations: Vec<ToolDecl>,
    allowed_names: BTreeSet<String>,
}

impl ActiveFacadeToolRegistry {
    fn base_registry(&self) -> Result<FacadeToolRegistry, ToolRuntimeError> {
        let context = self
            .context
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
            .ok_or_else(|| ToolRuntimeError::InvalidRegistry {
                message: "facade tool registry context is not bound for this run".to_owned(),
            })?;
        FacadeToolRegistry::from_shared(
            self.tools.clone(),
            self.custom.clone(),
            self.extra.clone(),
            context,
        )
        .map_err(|error| ToolRuntimeError::InvalidRegistry {
            message: error.to_string(),
        })
    }
}

impl fmt::Debug for ActiveFacadeToolRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActiveFacadeToolRegistry")
            .field("allowed_names", &self.allowed_names)
            .field("has_custom", &self.custom.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolRegistry for ActiveFacadeToolRegistry {
    fn declarations(&self) -> Vec<ToolDecl> {
        self.declarations.clone()
    }

    async fn execute(
        &self,
        call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        if !self.allowed_names.contains(&call.name) {
            return Err(ToolRuntimeError::UnknownTool { name: call.name });
        }
        let registry = self.base_registry()?;
        registry.execute(call_id, call).await
    }
}
