use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use super::constraint::FieldMap;
use super::ResourceDef;

/// A request dispatched to a resource handler.
#[derive(Debug, Clone)]
pub struct OperationRequest {
    /// Operation name: "create", "list", "get", "delete", or custom
    pub operation: String,
    /// Resource name (for create/get/delete)
    pub name: Option<String>,
    /// Scope values (--org, --project, --vpc, etc.)
    pub scope: ScopeValues,
    /// All field values parsed from CLI args
    pub fields: FieldMap,
}

/// Scope values extracted from CLI flags.
#[derive(Debug, Clone, Default)]
pub struct ScopeValues {
    pub values: HashMap<String, String>,
}

impl ScopeValues {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }
}

/// A validated request — constraints have been checked, fields normalized.
/// Created by the dispatch layer after parsing + validation.
#[derive(Debug, Clone)]
pub struct ValidatedRequest {
    pub resource_kind: &'static str,
    pub operation: String,
    pub name: Option<String>,
    pub scope: ScopeValues,
    pub fields: FieldMap,
}

impl ValidatedRequest {
    /// Create from a raw request after validation succeeds.
    pub fn from_raw(kind: &'static str, req: OperationRequest) -> Self {
        Self {
            resource_kind: kind,
            operation: req.operation,
            name: req.name,
            scope: req.scope,
            fields: req.fields,
        }
    }
}

/// Response from a resource handler.
#[derive(Debug, Clone)]
pub enum OperationResponse {
    /// A single resource as JSON
    Resource(serde_json::Value),
    /// A list of resources as JSON array
    ResourceList(Vec<serde_json::Value>),
    /// A simple message
    Message(String),
    /// No output needed
    None,
}

/// Handler function type — processes an operation request.
pub type HandlerFn = Box<
    dyn Fn(
            OperationRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<OperationResponse>> + Send>>
        + Send
        + Sync,
>;

/// A complete resource registration: definition + handler.
pub struct ResourceRegistration {
    pub def: ResourceDef,
    pub handler: HandlerFn,
}

/// The resource registry — holds all registered resources.
pub struct ResourceRegistry {
    resources: Vec<ResourceRegistration>,
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self {
            resources: Vec::new(),
        }
    }

    /// Register a resource with its handler.
    pub fn register(&mut self, reg: ResourceRegistration) {
        self.resources.push(reg);
    }

    /// Find a resource by CLI name or alias.
    pub fn find(&self, name: &str) -> Option<&ResourceRegistration> {
        self.resources
            .iter()
            .find(|r| r.def.identity.cli_name == name || r.def.identity.aliases.contains(&name))
    }

    /// Iterate over all registered resources.
    pub fn iter(&self) -> impl Iterator<Item = &ResourceRegistration> {
        self.resources.iter()
    }

    /// Get all registrations as a slice.
    pub fn as_slice(&self) -> &[ResourceRegistration] {
        &self.resources
    }

    /// Consume the registry and return the registrations.
    pub fn into_registrations(self) -> Vec<ResourceRegistration> {
        self.resources
    }

    /// Number of registered resources.
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

impl Default for ResourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}
