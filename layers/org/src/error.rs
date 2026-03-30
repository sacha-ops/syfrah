/// Errors that can occur in org operations.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    #[error("org already exists: {0}")]
    AlreadyExists(String),

    #[error("org not found: {0}")]
    NotFound(String),

    #[error("org has projects and cannot be deleted: {0}")]
    OrgHasProjects(String),

    #[error("project already exists: {project} in org {org}")]
    ProjectAlreadyExists { org: String, project: String },

    #[error("project not found: {project} in org {org}")]
    ProjectNotFound { org: String, project: String },

    #[error("project has environments and cannot be deleted: {project} in org {org}")]
    ProjectHasEnvironments { org: String, project: String },

    #[error("invalid org name: {0}")]
    InvalidName(String),

    #[error("store error: {0}")]
    StoreError(String),
}

impl From<syfrah_state::StateError> for OrgError {
    fn from(e: syfrah_state::StateError) -> Self {
        OrgError::StoreError(e.to_string())
    }
}

/// Result type for org operations.
pub type Result<T> = std::result::Result<T, OrgError>;
