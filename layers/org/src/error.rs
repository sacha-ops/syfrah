/// Errors for the org layer.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    #[error("organization already exists: {0}")]
    OrgAlreadyExists(String),

    #[error("organization not found: {0}")]
    OrgNotFound(String),

    #[error("project already exists: {0}")]
    ProjectAlreadyExists(String),

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    #[error("environment already exists: {0}")]
    EnvAlreadyExists(String),

    #[error("environment not found: {0}")]
    EnvNotFound(String),

    #[error("environment is protected from deletion: {0}")]
    EnvProtected(String),

    #[error("state error: {0}")]
    State(#[from] syfrah_state::StateError),
}

pub type Result<T> = std::result::Result<T, OrgError>;
