use thiserror::Error;

/// Errors from the org layer.
#[derive(Debug, Error)]
pub enum OrgError {
    #[error("organization '{0}' already exists")]
    OrgAlreadyExists(String),

    #[error("organization '{0}' not found")]
    OrgNotFound(String),

    #[error("organization '{0}' has projects and cannot be deleted")]
    OrgHasProjects(String),

    #[error("project '{project}' already exists in org '{org}'")]
    ProjectAlreadyExists { org: String, project: String },

    #[error("project '{project}' not found in org '{org}'")]
    ProjectNotFound { org: String, project: String },

    #[error("project '{project}' in org '{org}' has environments and cannot be deleted")]
    ProjectHasEnvironments { org: String, project: String },

    #[error("invalid name: {0}")]
    InvalidName(String),

    #[error("storage error: {0}")]
    Store(#[from] syfrah_state::StateError),
}

pub type Result<T> = std::result::Result<T, OrgError>;
