/// Errors that can occur in overlay operations.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("interface already exists: {0}")]
    InterfaceExists(String),

    #[error("interface not found: {0}")]
    InterfaceNotFound(String),

    #[error("bridge not found: {0}")]
    BridgeNotFound(String),

    #[error("command failed: {0}")]
    CommandFailed(String),
}

/// Result type for overlay operations.
pub type Result<T> = std::result::Result<T, OverlayError>;
