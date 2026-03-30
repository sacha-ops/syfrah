/// Errors that can occur in org operations.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    #[error("org already exists: {0}")]
    AlreadyExists(String),

    #[error("org not found: {0}")]
    NotFound(String),

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
