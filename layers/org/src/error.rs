//! Error types for the org layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrgError {
    #[error("organization '{0}' already exists")]
    OrgExists(String),

    #[error("organization '{0}' not found")]
    OrgNotFound(String),

    #[error("project '{0}' already exists in organization '{1}'")]
    ProjectExists(String, String),

    #[error("project '{0}' not found in organization '{1}'")]
    ProjectNotFound(String, String),

    #[error("environment '{0}' already exists in project '{1}'")]
    EnvExists(String, String),

    #[error("environment '{0}' not found in project '{1}'")]
    EnvNotFound(String, String),

    #[error("environment '{0}' has deletion protection enabled. Run: syfrah env update {0} --project {1} --org {2} --no-deletion-protection")]
    EnvProtected(String, String, String),

    #[error("organization '{0}' has {1} project(s) — delete them first")]
    OrgNotEmpty(String, usize),

    #[error("project '{0}' has {1} environment(s) — delete them first")]
    ProjectNotEmpty(String, usize),

    #[error("invalid name '{0}': {1}")]
    InvalidName(String, String),

    #[error("state error: {0}")]
    State(#[from] syfrah_state::StateError),
}
