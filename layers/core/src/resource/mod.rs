//! Declarative resource framework for Syfrah.
//!
//! Every cloud resource (VPC, VM, hypervisor, etc.) is defined as a [`ResourceDef`]
//! which describes its identity, scope, schema, operations, and presentation.
//! The CLI and API are generated automatically from these definitions.

mod builder;
mod cli_gen;
mod constraint;
mod dispatch;
mod identity;
mod operation;
mod presentation;
mod registry;
mod schema;
mod scope;

pub use builder::*;
pub use cli_gen::*;
pub use constraint::*;
pub use dispatch::*;
pub use identity::*;
pub use operation::*;
pub use presentation::*;
pub use registry::*;
pub use schema::*;
pub use scope::*;

/// The complete definition of a cloud resource.
/// This is the single source of truth from which CLI, API, validation,
/// and documentation are all derived.
#[derive(Debug, Clone)]
pub struct ResourceDef {
    pub identity: ResourceIdentity,
    pub scope: ScopeDef,
    pub schema: ResourceSchema,
    pub operations: Vec<OperationDef>,
    pub presentation: PresentationDef,
}
