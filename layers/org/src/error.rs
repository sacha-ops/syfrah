/// Errors from the org layer.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    #[error("state error: {0}")]
    State(#[from] syfrah_state::StateError),
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, OrgError>;
